//! Choose the Python used by this vault's notebooks, debugger and language server, through the
//! Code pane's Python Environment dropdown (`sidebar::python`).

use accent_api::python::Environments;
use sourceview5::prelude::BufferExt;
use std::rc::Rc;

use crate::App;
use crate::doc::Doc;

impl App {
    /// The environments for the dropdown, found for the Code pane's Python file or notebook,
    /// whose project may name its own for Pyright; without one the vault's root stands in, which
    /// names no project.
    pub(crate) fn python_environments(
        self: &Rc<Self>,
        then: Box<dyn FnOnce(Result<Environments, String>)>,
    ) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let rel = match self.code_doc() {
            Some(Doc::Text(tab))
                if tab
                    .buffer
                    .language()
                    .is_some_and(|language| language.id() == "python3") =>
            {
                tab.rel()
            }
            Some(Doc::Notebook(tab)) if tab.is_python() => tab.key(),
            _ => String::new(),
        };
        let asked = vault.python_environments(&rel);
        gtk::glib::spawn_future_local(async move {
            then(asked.await.map_err(|error| error.to_string()));
        });
    }

    /// Keep `command` as the vault's Python, empty for the default configuration.
    pub(crate) fn pick_python(&self, command: Vec<String>) {
        let root = self.root();
        let mut config = self.config.borrow_mut();
        let mut vault = config.vault(&root);
        if vault.debug.python != command {
            vault.debug.python = command;
            config.set_vault(&root, vault);
            drop(config);
            self.config_changed();
        }
    }

    /// Select Python Environment: the Code pane's dropdown, opened, while a strip holds one.
    pub(crate) fn choose_python_environment(self: &Rc<Self>) {
        if self.vault().is_none() {
            return self.needs_vault("select a Python environment");
        }
        if self.debugger.running() {
            return self.toast("Select a Python environment once the debug session ends");
        }
        let Some(button) = self.sidebar.get().and_then(|s| s.python_button()) else {
            return self.toast("Open a Python file or notebook to select its environment");
        };
        self.show_pane("code");
        button.popup();
    }
}
