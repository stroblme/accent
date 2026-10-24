//! The Code pane's Python Environment dropdown: the interpreter the vault's notebooks, debugger
//! and Python language analysis use, picked from those found on the vault's machine.

use accent_api::python::{Environment, Environments};
use gtk::pango;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// What the dropdown asks of the window, as closures so the pane never sees a vault.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// The environments for the Python tab in front, handed back once found, or why not.
    pub list: Rc<dyn Fn(Box<dyn FnOnce(Result<Environments, String>)>)>,
    /// The command picked, empty for the default configuration.
    pub pick: Rc<dyn Fn(Vec<String>)>,
}

/// A flat menu button naming the chosen interpreter, as the Git pane's branch is, over a popover
/// listing the choices, asked for afresh each time it opens: a venv made since is found.
#[derive(Clone)]
pub(super) struct Picker {
    pub(super) button: gtk::MenuButton,
    name: gtk::Label,
}

impl Picker {
    pub(super) fn new(data: &Rc<Data>) -> Picker {
        // Ellipsized as the branch's name is, so a long path does not decide how narrow the
        // sidebar can be.
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        face.add_css_class("heading");
        face.append(&name);
        // `go-down-symbolic` as the branch button's, for WhiteSur (`git::build_branch_row`).
        face.append(&gtk::Image::from_icon_name("go-down-symbolic"));

        let rows = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        rows.add_css_class("navigation-sidebar");
        let notes = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(280)
                .child(&rows)
                .build(),
        );
        content.append(&notes);
        let menu = gtk::Popover::builder().child(&content).build();
        // Regular weight, as the branch popover's rows (`install_chrome_css`).
        menu.add_css_class("python-environments");
        let button = gtk::MenuButton::builder()
            .hexpand(true)
            .halign(gtk::Align::Start)
            .child(&face)
            .popover(&menu)
            .build();
        button.add_css_class("flat");

        // Only the last opening's answer is shown: a slow probe must not land over a later list.
        let asked = Rc::new(Cell::new(0u32));
        let data = data.clone();
        menu.connect_show(move |menu| {
            let n = asked.get() + 1;
            asked.set(n);
            clear(&rows, &notes);
            notes.append(
                &adw::Spinner::builder()
                    .margin_top(12)
                    .margin_bottom(6)
                    .build(),
            );
            let (asked, rows, notes, menu) =
                (asked.clone(), rows.clone(), notes.clone(), menu.downgrade());
            let pick = data.pick.clone();
            (data.list)(Box::new(move |answer| {
                if asked.get() == n
                    && let Some(menu) = menu.upgrade()
                {
                    fill(&menu, &rows, &notes, answer, &pick);
                }
            }));
        });
        Picker { button, name }
    }

    /// Name `command`, the vault's choice, on the button.
    pub(super) fn set(&self, command: &[String]) {
        let face = face(command);
        self.button
            .set_tooltip_text(Some(&format!("Python Environment: {face}")));
        self.name.set_text(&face);
    }

    /// Show the chevron alone (`compact`), for a row the session's steps fill: an ellipsis is
    /// all the name would keep there, and its width would widen the sidebar.
    pub(super) fn set_compact(&self, compact: bool) {
        self.name.set_visible(!compact);
    }
}

/// The button's words for `command`: the command as the list shows it, or "Default Python".
fn face(command: &[String]) -> String {
    match command.is_empty() {
        true => "Default Python".to_string(),
        false => command.join(" "),
    }
}

/// The popover's rows for `answer`, the chosen one checked and given the keyboard as a dropdown
/// opens on its selection, and under them what holds beside the choice.
fn fill(
    menu: &gtk::Popover,
    rows: &gtk::ListBox,
    notes: &gtk::Box,
    answer: Result<Environments, String>,
    pick: &Rc<dyn Fn(Vec<String>)>,
) {
    clear(rows, notes);
    let environments = match answer {
        Ok(environments) => environments,
        Err(why) => return notes.append(&note(&format!("Cannot list Python environments: {why}"))),
    };
    if environments.choices.len() == 1 {
        notes.append(&note(
            "No environment with ipykernel was found in the vault: install ipykernel in one, \
             then open this list again",
        ));
    }
    for Environment { command, label } in environments.choices {
        let chosen = command == environments.selected;
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        face.append(
            &gtk::Label::builder()
                .label(&label)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        if chosen {
            face.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        let button = gtk::Button::builder().child(&face).build();
        // A text button's inset, as the branch popover's rows have with a bare label for a child.
        button.add_css_class("text-button");
        button.add_css_class("flat");
        if command.is_empty() {
            button.set_tooltip_text(Some(
                "The global setting, else the nearest environment, else python3; notebooks need \
                 ipykernel",
            ));
        }
        let (menu, pick) = (menu.downgrade(), pick.clone());
        button.connect_clicked(move |_| {
            if let Some(menu) = menu.upgrade() {
                menu.popdown();
            }
            pick(command.clone());
        });
        rows.append(&button);
        if chosen {
            button.grab_focus();
        }
    }
    if let Some(path) = environments.pyright_override {
        notes.append(&note(&format!(
            "{path} overrides the selected environment for Pyright"
        )));
    }
    notes.append(&note(
        "Language analysis updates now; running kernels and debug sessions keep theirs until \
         restarted",
    ));
}

/// A dim line under the rows, wrapped to the popover's width rather than widening it.
fn note(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .build();
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label
}

fn clear(rows: &gtk::ListBox, notes: &gtk::Box) {
    while let Some(row) = rows.first_child() {
        rows.remove(&row);
    }
    while let Some(line) = notes.first_child() {
        notes.remove(&line);
    }
}
