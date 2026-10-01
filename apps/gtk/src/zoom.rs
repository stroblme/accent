//! Zoom, for a note, an image, a PDF and a shell alike: the step, its limits, the wheel and the
//! status bar readout.

use super::*;

/// One press of Zoom In or Zoom Out, in percent: a tenth of the document font.
const ZOOM_STEP: i64 = 10;

/// How long an image's zoom is left alone before an SVG is drawn again at it, the drawing before
/// enlarged meanwhile: a run of steps draws once, at the last.
const ZOOM_SETTLE: Duration = Duration::from_millis(300);

impl App {
    /// Step an image's zoom, or, with `None`, put it back to fitting the window.
    ///
    /// Reset is the fit, which is how the tab opened. The step is taken from the tab's own zoom
    /// rather than from the size it asked the picture for: a pixel width is a whole number, and a
    /// zoom read back out of one lands short of the tenth it was, which is enough for the next
    /// step to be the zoom the image is already at.
    ///
    /// A step keeps what is under `at` (a point in the scroller, the pointer's) where it is, as a
    /// PDF page does; a chord has no pointer and keeps the middle of the view, as a diagram does.
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
        let zoom = zoom.map(|zoom| zoom.clamp(pdfview::MIN_SCALE, pdfview::MAX_SCALE));
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

    /// Zoom is the document's, never the chrome's: DESIGN.md leaves the interface font to the
    /// system, and this is the reading size of one note. Presentation mode is the same WebView,
    /// so it is zoomed along with the preview.
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
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.set_zoom(zoom);
        }
        self.refresh_zoom();
        self.save_session_soon();
    }

    /// The zoom readout in the status bar: the document zoom for a text tab, the shell's own for a
    /// terminal, and the PDF's own for a PDF, which fits to the window rather than counting
    /// percentages.
    ///
    /// A document at 100 % has nothing to say, so the readout goes rather than leaving a control
    /// saying nothing is going on; the same for a shell at its own size. A PDF always shows one:
    /// fitting is a zoom too, and it is what clicking the readout goes back to. So does an image,
    /// for the same reason. A status page and a diff show nothing at all, because no zoom reaches
    /// them — the readout used to fall through to the window's document zoom and say "120 %" over
    /// a picture drawn at its own size. It matches the same six variants [`App::zoom_action`]
    /// does, so the readout and the chords cannot disagree.
    pub fn refresh_zoom(&self) {
        let label = match self.active_doc() {
            Some(Doc::Pdf(pdf)) => pdf.zoom_label(),
            Some(Doc::Diagram(d)) => Some(d.zoom_label()),
            Some(Doc::Terminal(term)) => term.zoom_label(),
            Some(Doc::Text(_)) | Some(Doc::Diff(_)) => {
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

/// Scroll an image's scroller so that what is under `at` is under it again once the picture in
/// it is `size`, by [`zoomed_scroll`].
///
/// The viewport takes the picture's new size only when it is next allocated, and meanwhile
/// clamps a value to the old one, so the adjustments are handed the new extent first: the
/// picture's, or the viewport's own while the picture is the smaller, as the viewport will.
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

/// Zoom in tenths, between half size and triple. Rounded as well as clamped, so stepping does
/// not drift into 0.7999999999999999 and a hand-edited state file cannot ask for 0.
pub fn clamp_zoom(zoom: f64) -> f64 {
    ((zoom * 10.0).round() / 10.0).clamp(0.5, 3.0)
}

/// One step in or out from `zoom`: the next multiple of [`ZOOM_STEP`], so a PDF fitted to the
/// window at 137 % lands on 140 % rather than 147 %. Shared with `pdfview`, so a chord, a wheel
/// notch and a pinch mean the same amount of zoom whichever kind of tab is in front.
///
/// Counted in whole percent, the readout's own unit. A zoom read back off a rendered size (an
/// `f32` layout scale, a pixel width) is a hair off the tenth it was; counted in fractions of a
/// step, a hair under made that tenth the next one, and the step returned its own input. Rounded
/// to a percent it is that tenth again, and a step always moves the zoom by half a percent at
/// least.
pub fn stepped_zoom(zoom: f64, out: bool) -> f64 {
    let percent = (zoom * 100.0).round() as i64;
    let next = match out {
        true => (percent - 1) / ZOOM_STEP * ZOOM_STEP,
        false => (percent / ZOOM_STEP + 1) * ZOOM_STEP,
    };
    next as f64 / 100.0
}

/// Where one axis scrolls to so that a zoom keeps what is under the pointer under it: the point
/// `at` into the viewport, scrolled to `offset`, stays the same fraction of the content as that
/// grows from `was` long to `now`. Clamping is the adjustment's, since a point near the edge of
/// content smaller than the viewport cannot stay put.
pub fn zoomed_scroll(at: f64, offset: f64, was: f64, now: f64) -> f64 {
    (offset + at) * now / was.max(1.0) - at
}

/// How many whole steps `dy` completes, given the fraction earlier deltas left over. A
/// smooth-scroll device sends one wheel notch as several fractional deltas, and one notch is one
/// step wherever the wheel zooms.
pub fn wheel_steps(accum: &Cell<f64>, dy: f64) -> i32 {
    let total = accum.get() + dy;
    accum.set(total.fract());
    total.trunc() as i32
}

/// Ctrl+scroll on `widget` steps whatever it is that zooms there: `step(true, at)` is one step
/// out, `step(false, at)` one step in. One notch is one step, the same amount the chords move.
///
/// `at` is where the pointer is in the widget, for a zoom that keeps what is under it where it
/// is — which is what a PDF page does and what a font size has no use for. A controller of its
/// own tracks it, because a scroll event carries no widget coordinate.
///
/// Each controller owns its own accumulator, because a smooth-scroll device sends one notch as
/// several fractional deltas and two widgets sharing the remainder would zoom each other. Without
/// Control the event is passed on untouched, so a plain scroll still scrolls whatever it scrolled.
///
/// The phase is the caller's. A text view wants `Bubble`, ahead of the scrolled window around it;
/// WebKit and VTE answer a Ctrl+scroll themselves, with a zoom of their own that neither the
/// readout nor the session would know about, so those two have to be beaten to it in `Capture`.
pub fn zoom_on_wheel(
    widget: &impl IsA<gtk::Widget>,
    phase: gtk::PropagationPhase,
    step: impl Fn(bool, Option<(f64, f64)>) + 'static,
) {
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

    let accum = Cell::new(0.0);
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(phase);
    wheel.connect_scroll(move |controller, _, dy| {
        if !controller
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let steps = wheel_steps(&accum, dy);
        for _ in 0..steps.abs() {
            step(steps > 0, at.get());
        }
        glib::Propagation::Stop
    });
    widget.add_controller(wheel);
}

/// The zoom a pinch has reached: the zoom it began at times `scale`, on the nearest tenth, the
/// step every other zoom takes. GTK gives the scale against the fingers' spread when the pinch
/// began, so it is the whole pinch so far and never a step to add to the last one.
pub fn pinched_zoom(start: f64, scale: f64) -> f64 {
    (start * scale * 10.0).round() / 10.0
}

/// Two fingers on `widget` zoom it: `from()` is read as the pinch begins, and `to(zoom, at)` is
/// handed each new [`pinched_zoom`] with the point between the fingers, which is what stays put.
///
/// Nothing is handed on until the fingers reach another tenth, so a pinch re-lays the page out
/// once a tenth rather than on every event, and fingers resting on a page fitted at 137 % leave
/// it there rather than snapping it to 140 %.
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
        move |_, _| {
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
