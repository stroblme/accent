//! The window's toasts, several at once.
//!
//! `AdwToastOverlay` shows one toast and queues the rest, so a toast waited behind every one
//! before it, and a Replace All's Undo could still be waiting after a newer rewrite it would no
//! longer undo. Here up to [`MAX`] stand at the bottom centre, the newest on top, each going on
//! its own timeout; one more sends the oldest away, and a toast with a key takes the place of the
//! one standing under the same key. What is on screen is all the history there is.
//!
//! Each is drawn as libadwaita draws its own (`adw-toast-widget.ui`): a box under the `toast` CSS
//! name, which Adwaita's stylesheet paints, holding the title, an optional button and a close
//! button. Its timeout waits while the pointer is over it or it has the keyboard, Escape takes
//! the newest away, and it is announced as libadwaita announces its own.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// How many toasts stand at once.
const MAX: usize = 3;

/// What a toast says and offers.
pub struct Toast {
    title: String,
    button: Option<(String, Box<dyn Fn()>)>,
    /// Seconds before it goes by itself; 0 keeps it until it is dismissed.
    timeout: u32,
    key: Option<String>,
}

impl Toast {
    /// A toast saying `title`, which goes after libadwaita's 5 s.
    pub fn new(title: &str) -> Self {
        Toast {
            title: title.to_string(),
            button: None,
            timeout: 5,
            key: None,
        }
    }

    /// A button saying `label` that runs `act`, the toast going with the press.
    pub fn button(mut self, label: &str, act: impl Fn() + 'static) -> Self {
        self.button = Some((label.to_string(), Box::new(act)));
        self
    }

    /// What the toast is about: a newer one with the same key takes its place.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// A toast that stays until it is dismissed, for a drill that measures it.
    #[cfg(feature = "bench")]
    pub fn lasting(mut self) -> Self {
        self.timeout = 0;
        self
    }
}

/// The toasts over the document column.
pub struct Toasts {
    overlay: gtk::Overlay,
    pile: Pile,
    /// The toasts standing, newest first, each under its key.
    up: RefCell<Vec<(Option<String>, Rc<Up>)>>,
}

/// A toast standing: its widget, and the timeout that takes it away unless something holds it.
struct Up {
    revealer: gtk::Revealer,
    seconds: u32,
    timer: RefCell<Option<glib::SourceId>>,
    held: Cell<u32>,
}

impl Toasts {
    /// The toasts over `child`, which stand at the bottom of it.
    pub fn new(child: &impl IsA<gtk::Widget>) -> Rc<Self> {
        let pile: Pile = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 6)
            .property("halign", gtk::Align::Center)
            .property("valign", gtk::Align::End)
            .property("margin-start", 12)
            .property("margin-end", 12)
            .property("margin-bottom", 24)
            .build();
        pile.add_css_class("accent-toasts");
        let overlay = gtk::Overlay::builder().child(child).build();
        overlay.add_overlay(&pile);
        let toasts = Rc::new(Toasts {
            overlay,
            pile,
            up: RefCell::default(),
        });
        // Escape takes the newest away, as it takes libadwaita's one toast, and goes on to the
        // window when none stands.
        let weak = Rc::downgrade(&toasts);
        let escape = gtk::ShortcutController::new();
        escape.add_shortcut(gtk::Shortcut::new(
            Some(gtk::KeyvalTrigger::new(
                gdk::Key::Escape,
                gdk::ModifierType::empty(),
            )),
            Some(gtk::CallbackAction::new(move |_, _| {
                let Some(toasts) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let newest = toasts.up.borrow().first().map(|(_, up)| up.clone());
                match newest {
                    Some(up) => {
                        toasts.dismiss(&up);
                        glib::Propagation::Stop
                    }
                    None => glib::Propagation::Proceed,
                }
            })),
        ));
        toasts.overlay.add_controller(escape);
        toasts
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.overlay
    }

    /// Put `toast` on top of the pile.
    pub fn add(self: &Rc<Self>, toast: Toast) {
        let announcement = match &toast.button {
            Some((label, _)) => format!("A toast appeared: {}, has a button: {label}", toast.title),
            None => format!("A toast appeared: {}", toast.title),
        };
        let up = self.draw(toast.title, toast.button, toast.timeout);
        let gone = push(&mut self.up.borrow_mut(), toast.key, up.clone());
        for up in gone {
            hide(&up);
        }
        self.pile.prepend(&up.revealer);
        up.revealer.set_reveal_child(true);
        self.start(&up);
        self.overlay
            .announce(&announcement, gtk::AccessibleAnnouncementPriority::Medium);
    }

    /// Take every toast away, for a drill that needs none standing.
    #[cfg(feature = "bench")]
    pub fn dismiss_all(&self) {
        for (_, up) in self.up.take() {
            hide(&up);
        }
    }

    /// The titles of the toasts standing, top to bottom, as the pile draws them.
    #[cfg(feature = "bench")]
    pub fn shown(&self) -> Vec<String> {
        let mut titles = Vec::new();
        let mut child = self.pile.first_child().and_downcast::<gtk::Revealer>();
        while let Some(revealer) = child {
            let title = revealer.child().and_then(|toast| toast.first_child());
            if let Some(title) = title.and_downcast::<gtk::Label>()
                && revealer.reveals_child()
            {
                titles.push(title.label().to_string());
            }
            child = revealer.next_sibling().and_downcast();
        }
        titles
    }

    /// One toast's widgets, wired to take it away: the close button, the button's press and
    /// the timeout, which the pointer over it and the keyboard in it hold off.
    fn draw(
        self: &Rc<Self>,
        title: String,
        button: Option<(String, Box<dyn Fn()>)>,
        seconds: u32,
    ) -> Rc<Up> {
        let toast = gtk::Box::builder()
            .css_name("toast")
            .accessible_role(gtk::AccessibleRole::Alert)
            .build();
        toast.append(
            &gtk::Label::builder()
                .label(title)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .single_line_mode(true)
                .xalign(0.0)
                .hexpand(true)
                .margin_start(6)
                .margin_end(6)
                .css_classes(["heading"])
                .build(),
        );
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(crate::widgets::FADE_MS)
            .halign(gtk::Align::Center)
            .child(&toast)
            .build();
        let up = Rc::new(Up {
            revealer,
            seconds,
            timer: RefCell::default(),
            held: Cell::new(0),
        });
        let (toasts, weak) = (Rc::downgrade(self), Rc::downgrade(&up));
        let dismiss = move || {
            if let (Some(toasts), Some(up)) = (toasts.upgrade(), weak.upgrade()) {
                toasts.dismiss(&up);
            }
        };
        if let Some((label, act)) = button {
            let pressed = gtk::Button::builder()
                .label(label)
                .use_underline(true)
                .valign(gtk::Align::Center)
                .focus_on_click(false)
                .can_shrink(true)
                .build();
            let dismiss = dismiss.clone();
            pressed.connect_clicked(move |_| {
                act();
                dismiss();
            });
            toast.append(&pressed);
        }
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Dismiss")
            .valign(gtk::Align::Center)
            .focus_on_click(false)
            .css_classes(["circular", "flat"])
            .build();
        close.connect_clicked(move |_| dismiss());
        toast.append(&close);

        let hold = {
            let (toasts, weak) = (Rc::downgrade(self), Rc::downgrade(&up));
            move |on: bool| {
                if let (Some(toasts), Some(up)) = (toasts.upgrade(), weak.upgrade()) {
                    toasts.hold(&up, on);
                }
            }
        };
        let motion = gtk::EventControllerMotion::new();
        let (enter, leave) = (hold.clone(), hold.clone());
        motion.connect_enter(move |_, _, _| enter(true));
        motion.connect_leave(move |_| leave(false));
        let focus = gtk::EventControllerFocus::new();
        let enter = hold.clone();
        focus.connect_enter(move |_| enter(true));
        focus.connect_leave(move |_| hold(false));
        toast.add_controller(motion);
        toast.add_controller(focus);

        // Gone from the pile once it has slid away, at once where animations are off.
        up.revealer.connect_child_revealed_notify(|revealer| {
            if !revealer.reveals_child()
                && !revealer.is_child_revealed()
                && let Some(pile) = revealer.parent().and_downcast::<gtk::Box>()
            {
                pile.remove(revealer);
            }
        });
        up
    }

    fn dismiss(&self, up: &Rc<Up>) {
        self.up
            .borrow_mut()
            .retain(|(_, standing)| !Rc::ptr_eq(standing, up));
        hide(up);
    }

    /// Hold `up`'s timeout while `on`, and start it again once nothing holds it.
    fn hold(self: &Rc<Self>, up: &Rc<Up>, on: bool) {
        match on {
            true => {
                up.held.set(up.held.get() + 1);
                if let Some(timer) = up.timer.take() {
                    timer.remove();
                }
            }
            false => {
                up.held.set(up.held.get().saturating_sub(1));
                if up.held.get() == 0 {
                    self.start(up);
                }
            }
        }
    }

    fn start(self: &Rc<Self>, up: &Rc<Up>) {
        if up.seconds == 0 || up.timer.borrow().is_some() {
            return;
        }
        let (toasts, weak) = (Rc::downgrade(self), Rc::downgrade(up));
        let timer =
            glib::timeout_add_local_once(Duration::from_secs(up.seconds.into()), move || {
                if let (Some(toasts), Some(up)) = (toasts.upgrade(), weak.upgrade()) {
                    up.timer.take();
                    toasts.dismiss(&up);
                }
            });
        up.timer.replace(Some(timer));
    }
}

/// Slide `up` away, its timeout with it.
fn hide(up: &Up) {
    if let Some(timer) = up.timer.take() {
        timer.remove();
    }
    up.revealer.set_can_target(false);
    up.revealer.set_reveal_child(false);
}

/// Put `new` on top of `up`, newest first, and take off what it displaces: the toast standing
/// under its key, then the oldest past [`MAX`].
fn push<T>(up: &mut Vec<(Option<String>, T)>, key: Option<String>, new: T) -> Vec<T> {
    let mut gone = Vec::new();
    if let Some(at) = up.iter().position(|(k, _)| key.is_some() && *k == key) {
        gone.push(up.remove(at).1);
    }
    up.insert(0, (key, new));
    gone.extend(up.drain(MAX.min(up.len())..).map(|(_, t)| t));
    gone
}

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct Pile;

    #[glib::object_subclass]
    impl ObjectSubclass for Pile {
        const NAME: &'static str = "AccentToastPile";
        type Type = super::Pile;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for Pile {}

    /// Only the toasts take the pointer: the gaps between and beside them are the document's,
    /// as the space around libadwaita's one toast is.
    impl WidgetImpl for Pile {
        fn contains(&self, _x: f64, _y: f64) -> bool {
            false
        }
    }

    impl BoxImpl for Pile {}
}

glib::wrapper! {
    /// The column the toasts stand in.
    pub struct Pile(ObjectSubclass<imp::Pile>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_goes_on_top_and_displaces_the_oldest_or_its_key() {
        let mut up = Vec::new();
        for title in ["one", "two", "three", "four"] {
            let key = (title == "two").then(|| "replace".to_string());
            let gone = push(&mut up, key, title);
            assert_eq!(gone, if title == "four" { vec!["one"] } else { vec![] });
        }
        assert_eq!(push(&mut up, Some("replace".into()), "five"), vec!["two"]);
        let titles: Vec<_> = up.iter().map(|(_, t)| *t).collect();
        assert_eq!(titles, ["five", "four", "three"]);
    }
}
