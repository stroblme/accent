//! Two behaviours the window's dividers are missing: a double-click resets one to its default
//! position, and the line thickens while it is being dragged so the pointer has something to
//! aim at.
//!
//! `GtkPaned` offers neither. It sets no state flag and no style class while its handle is
//! dragged, and it has no reset gesture. Its own drag gesture runs in the capture phase *on the
//! paned* and claims the sequence, so a `GtkGestureClick` added next to it never sees the second
//! press. A [`gtk::EventControllerLegacy`] does: a claimed sequence cancels gestures, not raw
//! event controllers, so this one keeps receiving presses and releases while a drag is running.

use gtk::prelude::*;
use gtk::{gdk, glib};

/// The class the CSS in `main::install_chrome_css` widens the line for.
const DRAGGING: &str = "dragging";

/// One button press on a handle, in window coordinates and GDK's own event time (milliseconds),
/// which is what `gtk-double-click-time` is measured in.
#[derive(Clone, Copy, Debug)]
pub struct Click {
    pub x: f64,
    pub y: f64,
    pub time: u32,
}

/// Whether `now` completes a double-click begun by `prev`: soon enough and close enough, by the
/// user's own GTK settings. The pure half of [`watch`], and the only part worth a test.
pub fn is_double(prev: Click, now: Click, within_ms: u32, within_px: f64) -> bool {
    now.time.saturating_sub(prev.time) <= within_ms
        && (now.x - prev.x).abs() <= within_px
        && (now.y - prev.y).abs() <= within_px
}

/// Watch every paned in `window`. `reset` is called with the paned whose handle was
/// double-clicked and decides where its default position is.
pub fn watch(window: &gtk::Window, reset: impl Fn(&gtk::Paned) + 'static) {
    let controller = gtk::EventControllerLegacy::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    // `last` remembers the press a double-click would complete; `dragging` remembers the paned
    // that is wearing the class, so the release takes it off again even if the pointer has
    // meanwhile left the handle.
    let last: std::cell::RefCell<Option<(gtk::Paned, Click)>> = std::cell::RefCell::new(None);
    let dragging: std::cell::RefCell<Option<gtk::Paned>> = std::cell::RefCell::new(None);
    controller.connect_event(glib::clone!(
        #[weak]
        window,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, event| {
            match event.event_type() {
                gdk::EventType::ButtonPress => {
                    let on_handle = press(&window, event)
                        .and_then(|now| Some((handle_at(&window, now.x, now.y)?, now)));
                    let Some((paned, now)) = on_handle else {
                        // A press anywhere else ends the pending pair, so two presses on the
                        // same handle with a click in the document between them are two clicks.
                        *last.borrow_mut() = None;
                        return glib::Propagation::Proceed;
                    };
                    paned.add_css_class(DRAGGING);
                    *dragging.borrow_mut() = Some(paned.clone());
                    let settings = window.settings();
                    let time = u32::try_from(settings.gtk_double_click_time()).unwrap_or(400);
                    let distance = f64::from(settings.gtk_double_click_distance());
                    if let Some((before, first)) = last.replace(Some((paned.clone(), now)))
                        && before == paned
                        && is_double(first, now, time, distance)
                    {
                        reset(&paned);
                        // A third press starts a new pair rather than resetting again.
                        *last.borrow_mut() = None;
                    }
                }
                gdk::EventType::ButtonRelease => {
                    if let Some(paned) = dragging.borrow_mut().take() {
                        paned.remove_css_class(DRAGGING);
                    }
                }
                _ => {}
            }
            // Never swallow the event: the paned's own gesture still has to drive the drag.
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(controller);
}

/// A primary-button press, translated from the surface coordinates the raw event carries into
/// the window's own, which is what [`gtk::prelude::WidgetExt::pick`] wants.
fn press(window: &gtk::Window, event: &gdk::Event) -> Option<Click> {
    let button = event.downcast_ref::<gdk::ButtonEvent>()?;
    if button.button() != gdk::BUTTON_PRIMARY {
        return None;
    }
    let (x, y) = event.position()?;
    let (dx, dy) = window.surface_transform();
    Some(Click {
        x: x - dx,
        y: y - dy,
        time: event.time(),
    })
}

/// The paned whose drag handle sits at this point, if any.
///
/// The handle is a `GtkGizmo` child of the paned with no accessor of its own, so it is
/// identified by elimination: a pick that lands on a direct child of a paned which is neither of
/// its two panes can only be the handle. That holds for nested paneds too, where the inner one
/// *is* a pane of the outer.
fn handle_at(window: &gtk::Window, x: f64, y: f64) -> Option<gtk::Paned> {
    let picked = window.pick(x, y, gtk::PickFlags::DEFAULT)?;
    let paned = picked.parent()?.downcast::<gtk::Paned>().ok()?;
    let pane = [paned.start_child(), paned.end_child()]
        .iter()
        .any(|child| child.as_ref() == Some(&picked));
    (!pane).then_some(paned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(x: f64, y: f64, time: u32) -> Click {
        Click { x, y, time }
    }

    #[test]
    fn a_double_click_is_soon_and_in_place() {
        let first = click(100.0, 200.0, 1_000);
        assert!(is_double(first, click(101.0, 202.0, 1_150), 400, 5.0));
        assert!(
            !is_double(first, click(101.0, 202.0, 1_600), 400, 5.0),
            "too slow"
        );
        assert!(
            !is_double(first, click(120.0, 200.0, 1_150), 400, 5.0),
            "too far, which on a handle means the divider was dragged in between"
        );
    }
}
