//! Choose the Python used by this vault's notebooks, debugger and language server.

use accent_api::python::{Environment, Environments};
use adw::prelude::*;
use sourceview5::prelude::BufferExt;
use std::rc::Rc;

use crate::App;

impl App {
    pub(crate) fn choose_python_environment(self: &Rc<Self>) {
        let Some(vault) = self.vault().cloned() else {
            return self.needs_vault("select a Python environment");
        };
        let rel = self
            .active()
            .filter(|tab| {
                tab.buffer
                    .language()
                    .is_some_and(|language| language.id() == "python3")
            })
            .map(|tab| tab.rel())
            .or_else(|| {
                self.active_notebook()
                    .filter(|tab| tab.is_python())
                    .map(|tab| tab.key())
            });
        let Some(rel) = rel else {
            return self.toast("Open a Python file or notebook to select its environment");
        };
        let asked = vault.python_environments(&rel);
        let app = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let result = asked.await;
            let Some(app) = app.upgrade() else { return };
            match result {
                Ok(environments) => app.show_python_environments(environments),
                Err(error) => app.toast(&format!("Cannot list Python environments: {error}")),
            }
        });
    }

    fn show_python_environments(self: &Rc<Self>, environments: Environments) {
        let dialog = adw::PreferencesDialog::builder()
            .title("Python Environment")
            .build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Python environments")
            .description("Language analysis updates now; restart running kernels or debug sessions to use a new choice")
            .build();
        let selected = environments.selected;
        if environments.choices.len() == 1 {
            let row = adw::ActionRow::builder()
                .title("No Python environment with ipykernel was found")
                .subtitle("Install ipykernel in a vault environment, then reopen this picker")
                .build();
            group.add(&row);
        }
        for Environment { command, label } in environments.choices {
            let row = adw::ActionRow::builder()
                .title(&label)
                .activatable(true)
                .build();
            if command.is_empty() {
                row.set_subtitle(
                    "Global setting, nearest environment, or python3; notebooks need ipykernel",
                );
            }
            if command == selected {
                row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
            }
            let app = Rc::downgrade(self);
            let dialog = dialog.downgrade();
            row.connect_activated(move |_| {
                if let Some(app) = app.upgrade() {
                    let root = app.root();
                    let mut config = app.config.borrow_mut();
                    let mut vault = config.vault(&root);
                    if vault.debug.python != command {
                        vault.debug.python = command.clone();
                        config.set_vault(&root, vault);
                        drop(config);
                        app.config_changed();
                    }
                }
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
            group.add(&row);
        }
        page.add(&group);
        if let Some(path) = environments.pyright_override {
            let group = adw::PreferencesGroup::builder()
                .title("Language analysis")
                .description(format!(
                    "{path} overrides the selected environment for Pyright"
                ))
                .build();
            page.add(&group);
        }
        dialog.add(&page);
        crate::dialogs::close_on_outside_press(dialog.upcast_ref());
        dialog.present(Some(&self.window));
    }
}
