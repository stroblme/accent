//! Add double-click reset and drag feedback to window dividers.
//!
//! `GtkPaned` provides neither behaviour. Its capture-phase drag claims the sequence and
//! cancels neighbouring click gestures, so [`gtk::EventControllerLegacy`] observes raw
//! presses and releases even during a drag.

use gtk::prelude::*;
use gtk::{gdk, glib};

/// CSS class for wider dragged dividers and detecting user divider moves in Info.
pub(crate) const DRAGGING: &str = "dragging";

/// Handle press in window coordinates and GDK event milliseconds, matching GTK double-click settings.
#[derive(Clone, Copy, Debug)]
pub struct Click {
    pub x: f64,
    pub y: f64,
    pub time: u32,
}

/// Whether `now` completes `prev` within GTK's double-click time and distance limits.
pub fn is_double(prev: Click, now: Click, within_ms: u32, within_px: f64) -> bool {
    now.time.saturating_sub(prev.time) <= within_ms
        && (now.x - prev.x).abs() <= within_px
        && (now.y - prev.y).abs() <= within_px
}

/// Watch every paned in `window`. `reset` is called with the paned whose handle was
/// double-clicked and decides where its default position is.
pub fn watch(window: &gtk::Window, reset: impl Fn(&gtk::Paned) + 'static) {
    let reset: std::rc::Rc<dyn Fn(&gtk::Paned)> = std::rc::Rc::new(reset);
    let controller = gtk::EventControllerLegacy::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    // `last` remembers the press a double-click would complete; `dragging` remembers the paned
    // that is wearing the class, so the release takes it off again even if the pointer has
    // meanwhile left the handle.
    let last: std::cell::RefCell<Option<(gtk::Paned, Click)>> = std::cell::RefCell::new(None);
    let dragging: std::cell::RefCell<Option<gtk::Paned>> = std::cell::RefCell::new(None);
    // The paned a completed double-click is waiting to reset. The reset cannot happen on the
    // press that completes it: the paned's own drag gesture has already begun on that press and
    // recorded where the handle was, so the next motion writes the handle straight back to the
    // pointer and the default position is visible for a single frame. Holding it until the
    // release, and then one turn of the main loop later, lets that gesture finish first.
    let pending: std::cell::RefCell<Option<gtk::Paned>> = std::cell::RefCell::new(None);
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
                        *pending.borrow_mut() = Some(paned.clone());
                        // A third press starts a new pair rather than resetting again.
                        *last.borrow_mut() = None;
                    }
                }
                gdk::EventType::ButtonRelease => {
                    if let Some(paned) = dragging.borrow_mut().take() {
                        paned.remove_css_class(DRAGGING);
                    }
                    if let Some(paned) = pending.borrow_mut().take() {
                        let reset = reset.clone();
                        glib::idle_add_local_once(move || reset(&paned));
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

/// Find the paned whose handle contains this point.
///
/// The handle is a direct `GtkGizmo` child with no accessor. Excluding both panes identifies
/// it even in nested splits.
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
