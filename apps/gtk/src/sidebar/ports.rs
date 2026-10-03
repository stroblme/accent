//! The Ports pane: the forwards running over the window's ssh connection, and the row that
//! starts another one.

use crate::widgets::{scroller, status_page};
use accent_api::ssh::{Direction, Forward};
use adw::prelude::*;
use gtk::glib;
use gtk::pango;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Two machines wired together, which is what a forward is: a port on one cabled to a port on the
/// other. The rest of Adwaita's network names are signal strengths, a server tower or a VPN
/// shield — none of them a port.
pub(super) const ICON: &str = "network-wired-symbolic";

/// What the Ports pane asks of the ssh connection behind the vault. The first two are called on a
/// worker thread: ssh has to answer before either returns, and a host that is slow to would
/// otherwise hold the window.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// Start a forward, either way. Answers an error message when ssh refuses, which is what the
    /// pane shows.
    pub add_forward: Arc<dyn Fn(Forward) -> Result<(), String> + Send + Sync>,
    /// Take a forward down. Nothing to answer: the row goes either way.
    pub remove_forward: Arc<dyn Fn(Forward) + Send + Sync>,
    /// The forwards the connection keeps, which are the rows. Asks nothing of ssh.
    pub forwards: Box<dyn Fn() -> Vec<Forward>>,
}

/// What the two port boxes say, as a forward, or `None` while they are not one yet. `u16` does the
/// range check; port 0 is refused on top of it, because to the kernel it means "any free port" and
/// there is then nothing for the user to connect to.
fn ports(local: &str, remote: &str) -> Option<(u16, u16)> {
    let port = |text: &str| text.trim().parse::<u16>().ok().filter(|p| *p > 0);
    Some((port(local)?, port(remote)?))
}

/// Take one forward down: the forward, and the row it is drawn in.
type DropForward = Rc<dyn Fn(Forward, &gtk::ListBoxRow)>;

/// The forwards running over the window's ssh connection, and the row that starts another one;
/// and what draws the rows again from the connection's list, for a reconnect that dropped one.
///
/// The connection keeps the list (`Remote::forwards`), and the pane only draws it. Nothing asks
/// ssh what it has open, so what the user added is what is drawn; the connection puts them back
/// after a reconnect (`Remote::connect`), and drops one that will not come back.
pub(super) fn pane(data: &Rc<Data>) -> (gtk::Widget, Rc<dyn Fn()>) {
    // A `GtkListBox` rebuilt row by row rather than a list view and a factory: there are a handful
    // of forwards at most, so a model to recycle rows into would cost more than it saves.
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("navigation-sidebar");

    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(
        &status_page(
            ICON,
            "No Forwarded Ports",
            "A forward makes a port on one machine reachable on the other.",
        ),
        Some("empty"),
    );
    body.add_named(&scroller(&list), Some("list"));
    body.set_visible_child_name("empty");

    // A banner and not a toast (DESIGN.md, States): ssh refusing a port is not something that
    // happened and is over, it is the state of the forward that is not up, and the way out of it
    // is to choose another port — a decision made in the row right below the message.
    let banner = adw::Banner::new("");

    // The widgets are held weakly by every closure here: each closure ends up in a handler on a
    // widget inside the ones it names — a row's close button, the entries, Add — and a strong
    // handle would be a cycle keeping the pane, and the vault behind `data`, alive after the
    // window has closed.

    // The list, or the empty page while it has no rows.
    let show: Rc<dyn Fn()> = Rc::new(glib::clone!(
        #[weak]
        list,
        #[weak]
        body,
        move || {
            body.set_visible_child_name(match list.first_child() {
                Some(_) => "list",
                None => "empty",
            });
        }
    ));

    let drop_forward: DropForward = Rc::new({
        let (data, show) = (data.clone(), show.clone());
        move |f, row: &gtk::ListBoxRow| {
            // Greyed out while ssh answers, so the button cannot ask a second time.
            row.set_sensitive(false);
            let (remove, show, row) = (data.remove_forward.clone(), show.clone(), row.clone());
            glib::spawn_future_local(async move {
                // The row goes whatever ssh answers: a forward it would not cancel is one nothing
                // here could cancel either, and a master that died took its forwards with it.
                crate::work::off_thread("ssh", move || remove(f)).await;
                if let Some(list) = row.parent().and_downcast::<gtk::ListBox>() {
                    list.remove(&row);
                }
                show();
            });
        }
    });

    let refill: Rc<dyn Fn()> = Rc::new({
        let (data, drop_forward, show) = (data.clone(), drop_forward.clone(), show.clone());
        glib::clone!(
            #[weak]
            list,
            move || {
                list.remove_all();
                for f in (data.forwards)() {
                    list.append(&forward_row(f, drop_forward.clone()));
                }
                show();
            }
        )
    });

    let local = port_entry("Local");
    let remote = port_entry("Remote");
    // The arrow between the boxes is the direction, and pressing it flips it: the two ports stay
    // where they are, only which of them listens changes. Kept across Adds, so a run of forwards
    // the same way round is set once.
    let direction = Rc::new(Cell::new(Direction::ToRemote));
    let flip = gtk::Button::builder().valign(gtk::Align::Center).build();
    flip.add_css_class("flat");
    show_direction(&flip, direction.get());
    flip.connect_clicked({
        let direction = direction.clone();
        move |flip| {
            direction.set(match direction.get() {
                Direction::ToRemote => Direction::ToLocal,
                Direction::ToLocal => Direction::ToRemote,
            });
            show_direction(flip, direction.get());
        }
    });
    let add = gtk::Button::builder()
        .label("Add")
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();

    let submit: Rc<dyn Fn()> = Rc::new({
        let (data, refill) = (data.clone(), refill.clone());
        let direction = direction.clone();
        glib::clone!(
            #[weak]
            banner,
            #[weak]
            local,
            #[weak]
            remote,
            move || {
                let Some((from, to)) = ports(&local.text(), &remote.text()) else {
                    return;
                };
                let f = Forward {
                    local: from,
                    remote: to,
                    direction: direction.get(),
                };
                // ssh answers a forward it already has with OK and adds nothing, which would read
                // as a forward started.
                if (data.forwards)().contains(&f) {
                    banner.set_title(&format!("{f} is already forwarded"));
                    banner.set_revealed(true);
                    return;
                }
                let (banner, refill) = (banner.clone(), refill.clone());
                let (local, remote) = (local.clone(), remote.clone());
                let add = data.add_forward.clone();
                glib::spawn_future_local(async move {
                    let answered = crate::work::off_thread("ssh", move || add(f)).await;
                    match answered {
                        Some(Ok(())) => {
                            banner.set_revealed(false);
                            local.set_text("");
                            remote.set_text("");
                            refill();
                        }
                        Some(Err(message)) => {
                            banner.set_title(&message);
                            banner.set_revealed(true);
                        }
                        None => {
                            banner.set_title("Cannot forward this port");
                            banner.set_revealed(true);
                        }
                    }
                });
            }
        )
    });

    for entry in [&local, &remote] {
        entry.connect_changed(glib::clone!(
            #[weak]
            add,
            #[weak]
            local,
            #[weak]
            remote,
            move |_| add.set_sensitive(ports(&local.text(), &remote.text()).is_some())
        ));
        entry.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
    }
    add.connect_clicked({
        let submit = submit.clone();
        move |_| submit()
    });

    let entries = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    entries.append(&local);
    entries.append(&flip);
    entries.append(&remote);

    // The button on a line of its own, as the Search pane's replace row has it: the sidebar's
    // floor is 200 px, and two entries and a button do not share one line there.
    let form = gtk::Box::new(gtk::Orientation::Vertical, 6);
    form.set_margin_top(6);
    form.set_margin_bottom(6);
    form.set_margin_start(6);
    form.set_margin_end(6);
    form.append(&entries);
    form.append(&add);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&banner);
    column.append(&body);
    column.append(&form);
    (column.upcast(), refill)
}

/// Set the form's direction button to `direction`: its arrow, and the words for it.
fn show_direction(flip: &gtk::Button, direction: Direction) {
    flip.set_label(direction.arrow());
    flip.set_tooltip_text(Some(match direction {
        Direction::ToRemote => "A port on this machine reaches the remote one",
        Direction::ToLocal => "A port on the remote machine reaches this one",
    }));
}

/// A forward in words, naming both ends: the tooltip on its row.
fn describe(f: Forward) -> String {
    match f.direction {
        Direction::ToRemote => format!(
            "Port {} on this machine reaches port {} on the remote one",
            f.local, f.remote
        ),
        Direction::ToLocal => format!(
            "Port {} on the remote machine reaches port {} on this one",
            f.remote, f.local
        ),
    }
}

/// One live forward: its ports and direction, read-only, and the button that takes it down.
fn forward_row(f: Forward, drop_forward: DropForward) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(f.to_string())
        .tooltip_text(describe(f))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    label.add_css_class("numeric");
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Stop Forwarding")
        .valign(gtk::Align::Center)
        .build();
    close.add_css_class("flat");

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&label);
    content.append(&close);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(false)
        .build();
    // Weak: the button is inside the row it removes, and a strong handle would be a cycle no
    // amount of removing frees.
    close.connect_clicked(glib::clone!(
        #[weak]
        row,
        move |_| drop_forward(f, &row)
    ));
    row
}

/// A port box. Digits only, enforced on `insert-text` rather than through a `GtkEntryBuffer` of
/// our own: a buffer subclass is a GObject and a hundred lines for the same rule, while refusing
/// the insertion covers typing, pasting and a drop alike, all three arriving here as one.
fn port_entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .input_purpose(gtk::InputPurpose::Digits)
        .max_length(5)
        .width_chars(5)
        .hexpand(true)
        .build();
    entry.connect_insert_text(|entry, text, _| {
        if !text.chars().all(|c| c.is_ascii_digit()) {
            entry.stop_signal_emission_by_name("insert-text");
        }
    });
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_keeps_the_local_port_left_and_names_the_listening_end_first() {
        let out = Forward {
            local: 8080,
            remote: 80,
            direction: Direction::ToRemote,
        };
        let back = Forward {
            direction: Direction::ToLocal,
            ..out
        };
        assert_eq!(out.to_string(), "8080 → 80");
        assert_eq!(back.to_string(), "8080 ← 80");
        assert_eq!(
            describe(out),
            "Port 8080 on this machine reaches port 80 on the remote one"
        );
        assert_eq!(
            describe(back),
            "Port 80 on the remote machine reaches port 8080 on this one"
        );
    }

    #[test]
    fn a_forward_needs_two_real_port_numbers() {
        assert_eq!(ports("8080", "3000"), Some((8080, 3000)));
        assert_eq!(ports(" 22 ", "22"), Some((22, 22)));
        for (local, remote) in [
            ("", "3000"),
            ("8080", ""),
            ("0", "3000"),
            ("8080", "0"),
            ("http", "3000"),
            ("80.80", "3000"),
            ("-1", "3000"),
            ("65536", "3000"),
        ] {
            assert_eq!(ports(local, remote), None, "{local:?} -> {remote:?}");
        }
    }
}
