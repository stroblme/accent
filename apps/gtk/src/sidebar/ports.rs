//! The Ports pane: the forwards running over the window's ssh connection, and the row that
//! starts another one.

use crate::widgets::{scroller, status_page};
use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::pango;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Two machines wired together, which is what a forward is: a port on one cabled to a port on the
/// other. The rest of Adwaita's network names are signal strengths, a server tower or a VPN
/// shield — none of them a port.
pub(super) const ICON: &str = "network-wired-symbolic";

/// What the Ports pane asks of the ssh connection behind the vault. Both are called on a worker
/// thread: ssh has to answer before either returns, and a host that is slow to would otherwise
/// hold the window.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// Forward a remote port to a local one. Answers an error message when ssh refuses, which is
    /// what the pane shows.
    pub add_forward: Arc<dyn Fn(u16, u16) -> Result<(), String> + Send + Sync>,
    /// Take a forward down. Nothing to answer: the row goes either way.
    pub remove_forward: Arc<dyn Fn(u16, u16) + Send + Sync>,
}

/// What the two port boxes say, as a forward, or `None` while they are not one yet. `u16` does the
/// range check; port 0 is refused on top of it, because to the kernel it means "any free port" and
/// there is then nothing for the user to connect to.
fn ports(local: &str, remote: &str) -> Option<(u16, u16)> {
    let port = |text: &str| text.trim().parse::<u16>().ok().filter(|p| *p > 0);
    Some((port(local)?, port(remote)?))
}

/// Take one forward down: the two ports it carries, and the row it is drawn in.
type DropForward = Rc<dyn Fn(u16, u16, &gtk::ListBoxRow)>;

/// The forwards running over the window's ssh connection, and the row that starts another one.
///
/// The pane keeps the list itself. Nothing asks ssh what it has open, so what the user added is
/// what is drawn; the caller re-establishes them after a reconnect and the pane is only the list.
pub(super) fn pane(data: &Rc<Data>) -> gtk::Widget {
    let forwards: Rc<RefCell<Vec<(u16, u16)>>> = Rc::new(RefCell::new(Vec::new()));

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
            "A forward makes a port on the remote machine reachable at the same address on this one.",
        ),
        Some("empty"),
    );
    body.add_named(&scroller(&list), Some("list"));
    body.set_visible_child_name("empty");

    // A banner and not a toast (DESIGN.md, States): ssh refusing a port is not something that
    // happened and is over, it is the state of the forward that is not up, and the way out of it
    // is to choose another port — a decision made in the row right below the message.
    let banner = adw::Banner::new("");

    let switch_body: Rc<dyn Fn()> = Rc::new({
        let (body, forwards) = (body.clone(), forwards.clone());
        move || {
            body.set_visible_child_name(match forwards.borrow().is_empty() {
                true => "empty",
                false => "list",
            });
        }
    });

    let drop_forward: DropForward = Rc::new({
        let (data, forwards, list, switch_body) = (
            data.clone(),
            forwards.clone(),
            list.clone(),
            switch_body.clone(),
        );
        move |local, remote, row: &gtk::ListBoxRow| {
            // Greyed out while ssh answers, so the button cannot ask a second time.
            row.set_sensitive(false);
            let remove = data.remove_forward.clone();
            let (forwards, list, switch_body) =
                (forwards.clone(), list.clone(), switch_body.clone());
            let row = row.clone();
            glib::spawn_future_local(async move {
                // The row goes whatever ssh answers: a forward it would not cancel is one nothing
                // here could cancel either, and a master that died took its forwards with it.
                let _ = gio::spawn_blocking(move || remove(local, remote)).await;
                forwards.borrow_mut().retain(|f| *f != (local, remote));
                list.remove(&row);
                switch_body();
            });
        }
    });

    let local = port_entry("Local");
    let remote = port_entry("Remote");
    let add = gtk::Button::builder()
        .label("Add")
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();

    let submit: Rc<dyn Fn()> = Rc::new({
        let (data, forwards, list, banner, local, remote) = (
            data.clone(),
            forwards.clone(),
            list.clone(),
            banner.clone(),
            local.clone(),
            remote.clone(),
        );
        let (switch_body, drop_forward) = (switch_body.clone(), drop_forward.clone());
        move || {
            let Some((from, to)) = ports(&local.text(), &remote.text()) else {
                return;
            };
            let (forwards, list, banner) = (forwards.clone(), list.clone(), banner.clone());
            let (local, remote) = (local.clone(), remote.clone());
            let (switch_body, drop_forward) = (switch_body.clone(), drop_forward.clone());
            let add = data.add_forward.clone();
            glib::spawn_future_local(async move {
                let answered = gio::spawn_blocking(move || add(from, to)).await;
                match answered {
                    Ok(Ok(())) => {
                        banner.set_revealed(false);
                        forwards.borrow_mut().push((from, to));
                        list.append(&forward_row(from, to, drop_forward));
                        local.set_text("");
                        remote.set_text("");
                        switch_body();
                    }
                    Ok(Err(message)) => {
                        banner.set_title(&message);
                        banner.set_revealed(true);
                    }
                    Err(_) => {
                        banner.set_title("Cannot forward this port");
                        banner.set_revealed(true);
                    }
                }
            });
        }
    });

    for entry in [&local, &remote] {
        entry.connect_changed({
            let (add, local, remote) = (add.clone(), local.clone(), remote.clone());
            move |_| add.set_sensitive(ports(&local.text(), &remote.text()).is_some())
        });
        entry.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
    }
    add.connect_clicked({
        let submit = submit.clone();
        move |_| submit()
    });

    let arrow = gtk::Label::new(Some("→"));
    arrow.add_css_class("dim-label");
    let entries = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    entries.append(&local);
    entries.append(&arrow);
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
    column.upcast()
}

/// One live forward: `local → remote`, and the button that takes it down.
fn forward_row(local: u16, remote: u16, drop_forward: DropForward) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(format!("{local} → {remote}"))
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
        move |_| drop_forward(local, remote, &row)
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
