//! The Code pane's Python Environment dropdown: the interpreter the vault's notebooks, debugger
//! and Python language analysis use, picked from those found on the vault's machine. For a
//! notebook in another language it is that notebook's kernel instead, picked from the
//! kernelspecs of its language (user decision 2026-10-27).

use accent_api::kernel::Spec;
use accent_api::python::{Environment, Environments};
use gtk::pango;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What the dropdown asks of the window, as closures so the pane never sees a vault.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// The environments for the Python tab in front, handed back once found, or why not.
    pub list: Rc<dyn Fn(Box<dyn FnOnce(Result<Environments, String>)>)>,
    /// The command picked, empty for the default configuration.
    pub pick: Rc<dyn Fn(Vec<String>)>,
    /// The kernelspecs for the notebook in front, handed back once found, or why not.
    pub kernels: Rc<dyn Fn(Box<dyn FnOnce(Result<KernelChoices, String>)>)>,
    /// The kernelspec picked for the notebook in front, by name.
    pub pick_kernel: Rc<dyn Fn(String)>,
}

/// A notebook's language, the kernelspecs of it found and the one it runs on, and where they
/// were looked for, which is said when there are none.
pub struct KernelChoices {
    pub language: String,
    pub specs: Vec<Spec>,
    pub picked: Option<String>,
    pub searched: Vec<String>,
}

/// A flat menu button naming the chosen interpreter, as the Git pane's branch is, over a popover
/// listing the choices, asked for afresh each time it opens: a venv made since is found.
pub(super) struct Picker {
    /// Beside the strip's buttons, hidden until a strip is up and while a debug session runs.
    pub(super) button: gtk::MenuButton,
    name: gtk::Label,
    /// The vault's Python as the button names it.
    python: RefCell<String>,
    /// The kernel of the notebook in front as the button names it, while that notebook is in
    /// another language than Python: the popover then lists its language's kernelspecs.
    kernel: Rc<RefCell<Option<String>>>,
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
        // Level with the strip's buttons, at its 6 px inset, the caption under them hanging free.
        let button = gtk::MenuButton::builder()
            .valign(gtk::Align::Start)
            .margin_start(6)
            .margin_top(6)
            .visible(false)
            .child(&face)
            .popover(&menu)
            .build();
        button.add_css_class("flat");

        // Only the last opening's answer is shown: a slow probe must not land over a later list.
        let asked = Rc::new(Cell::new(0u32));
        let data = data.clone();
        let kernel = Rc::new(RefCell::new(None));
        let listing_kernels = kernel.clone();
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
            let shown = move |menu: &gtk::glib::WeakRef<gtk::Popover>| {
                (asked.get() == n).then(|| menu.upgrade()).flatten()
            };
            if listing_kernels.borrow().is_some() {
                let pick = data.pick_kernel.clone();
                return (data.kernels)(Box::new(move |answer| {
                    if let Some(menu) = shown(&menu) {
                        fill_kernels(&menu, &rows, &notes, answer, &pick);
                    }
                }));
            }
            let pick = data.pick.clone();
            (data.list)(Box::new(move |answer| {
                if let Some(menu) = shown(&menu) {
                    fill(&menu, &rows, &notes, answer, &pick);
                }
            }));
        });
        Picker {
            button,
            name,
            python: RefCell::default(),
            kernel,
        }
    }

    /// Name `command`, the vault's choice, on the button.
    pub(super) fn set(&self, command: &[String]) {
        self.python.replace(face(command));
        self.show();
    }

    /// Name `kernel`, the kernel of the notebook in front, on the button in the Python's place;
    /// `None` names the Python again.
    pub(super) fn set_kernel(&self, kernel: Option<&str>) {
        self.kernel.replace(kernel.map(String::from));
        self.show();
    }

    /// Whether the button is the vault's Python's, not a notebook's kernel's.
    pub(super) fn is_python(&self) -> bool {
        self.kernel.borrow().is_none()
    }

    fn show(&self) {
        let (face, tooltip) = match &*self.kernel.borrow() {
            Some(kernel) => (kernel.clone(), format!("Kernel: {kernel}")),
            None => {
                let python = self.python.borrow().clone();
                let tooltip = format!("Python Environment: {python}");
                (python, tooltip)
            }
        };
        self.button.set_tooltip_text(Some(&tooltip));
        self.name.set_text(&face);
    }

    /// What the button names, for the drills.
    #[cfg(feature = "bench")]
    pub(super) fn face(&self) -> String {
        self.name.text().to_string()
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
        let button = row(&label, chosen);
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

/// The popover's rows for a notebook's kernelspecs, the one it runs on checked, as [`fill`]'s.
fn fill_kernels(
    menu: &gtk::Popover,
    rows: &gtk::ListBox,
    notes: &gtk::Box,
    answer: Result<KernelChoices, String>,
    pick: &Rc<dyn Fn(String)>,
) {
    clear(rows, notes);
    let choices = match answer {
        Ok(choices) => choices,
        Err(why) => return notes.append(&note(&format!("Cannot list the kernels: {why}"))),
    };
    if choices.specs.is_empty() {
        let language = &choices.language;
        notes.append(&note(&format!(
            "No {language} kernel was found: install one, then open this list again"
        )));
        let searched = choices.searched.join(", ");
        return notes.append(&note(&format!("Kernels are read from {searched}")));
    }
    for spec in choices.specs {
        let chosen = choices.picked.as_deref() == Some(spec.name.as_str());
        let button = row(&spec.display_name, chosen);
        button.set_tooltip_text(Some(&format!("{} in {}", spec.name, spec.dir)));
        let (menu, pick) = (menu.downgrade(), pick.clone());
        button.connect_clicked(move |_| {
            if let Some(menu) = menu.upgrade() {
                menu.popdown();
            }
            pick(spec.name.clone());
        });
        rows.append(&button);
        if chosen {
            button.grab_focus();
        }
    }
    notes.append(&note("A running kernel keeps its own until restarted"));
}

/// A row of the popover, `label` and a check mark when `chosen`.
fn row(label: &str, chosen: bool) -> gtk::Button {
    let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    face.append(
        &gtk::Label::builder()
            .label(label)
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
    button
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
    label.add_css_class("dimmed");
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
