//! Zoom, for a note, an image, a PDF and a shell alike: the step, its limits, the wheel and the
//! status bar readout.

use super::*;

/// One press of Zoom In or Zoom Out, a tenth of the document font.
const ZOOM_STEP: f64 = 0.1;

impl App {
    /// Step an image's zoom, or, with `None`, put it back to fitting the window.
    ///
    /// Reset is the fit, which is how the tab opened, and is what a PDF's Fit Width is. The step
    /// is taken from the tab's own zoom rather than from the size it asked the picture for: a
    /// pixel width is a whole number, and a zoom read back out of one lands short of the tenth it
    /// was, which is enough for the next step to be the zoom the image is already at.
    pub fn zoom_image(self: &Rc<Self>, image: &doc::Viewer, out: Option<bool>) {
        let Some(picture) = picture_of(&image.page) else {
            return;
        };
        let zoom = out.zip(image_zoom(image, &picture)).map(|(out, from)| {
            stepped_zoom(from, out).clamp(pdfview::MIN_SCALE, pdfview::MAX_SCALE)
        });
        image.zoom.set(zoom);
        set_image_zoom(&picture, zoom);
        self.refresh_zoom();
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
        for doc in self.docs() {
            if let Some(diff) = doc.diff() {
                diff.set_font(font.as_deref(), zoom);
            }
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
fn image_zoom(image: &doc::Viewer, picture: &gtk::Picture) -> Option<f64> {
    if let Some(zoom) = image.zoom.get() {
        return Some(zoom);
    }
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

/// Draw an image at `zoom`, or fitted to the window when there is none.
///
/// A zoomed picture is centred and asks for its exact size, so the scroller scrolls it once it
/// is larger than the viewport and does not stretch it while it is smaller.
fn set_image_zoom(picture: &gtk::Picture, zoom: Option<f64>) {
    let size = zoom.and_then(|zoom| {
        let paintable = picture.paintable()?;
        let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
        (w > 0 && h > 0).then(|| ((f64::from(w) * zoom) as i32, (f64::from(h) * zoom) as i32))
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
/// The epsilon is what keeps an exact multiple from stepping to itself once the division has
/// drifted; the rounding is what keeps the result out of 1.4000000000000001.
pub fn stepped_zoom(zoom: f64, out: bool) -> f64 {
    let steps = zoom / ZOOM_STEP;
    let next = match out {
        true => (steps - 1e-6).ceil() - 1.0,
        false => (steps + 1e-6).floor() + 1.0,
    };
    (next * ZOOM_STEP * 100.0).round() / 100.0
}

/// How many whole steps `dy` completes, given the fraction earlier deltas left over. A
/// smooth-scroll device sends one wheel notch as several fractional deltas, and one notch is one
/// step wherever the wheel zooms.
pub fn wheel_steps(accum: &Cell<f64>, dy: f64) -> i32 {
    let total = accum.get() + dy;
    accum.set(total.fract());
    total.trunc() as i32
}

/// Ctrl+scroll on `widget` steps whatever it is that zooms there: `step(true)` is one step out,
/// `step(false)` one step in. One notch is one step, the same amount the chords move.
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
    step: impl Fn(bool) + 'static,
) {
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
            step(steps > 0);
        }
        glib::Propagation::Stop
    });
    widget.add_controller(wheel);
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
