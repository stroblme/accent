//! Bibliography surfaces share the text tab's buffer, save gate and undo history.
#[cfg(feature = "bench")]
pub mod bench;
mod edit;
mod import;
mod operations;
mod web;

use crate::{App, Opened, Tab, dialogs, work};
use accent_core::bib::{self, Library};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

pub fn is_bib(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|s| s.eq_ignore_ascii_case("bib"))
}

/// Virtualized rows shared by the sidebar and the full bibliography tab.
struct Entries {
    root: gtk::Box,
    search: gtk::SearchEntry,
    model: gtk::StringList,
    selection: gtk::MultiSelection,
    view: gtk::ListView,
    indices: RefCell<Vec<usize>>,
    library: RefCell<Option<Library>>,
    message: gtk::Label,
}
impl Entries {
    fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search References")
            .margin_start(6)
            .margin_end(6)
            .build();
        root.append(&search);
        let model = gtk::StringList::new(&[]);
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .lines(3)
                .margin_start(12)
                .margin_end(12)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            item.set_child(Some(&label));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let (Some(label), Some(text)) = (
                item.child().and_downcast::<gtk::Label>(),
                item.item().and_downcast::<gtk::StringObject>(),
            ) {
                label.set_text(&text.string());
            }
        });
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.set_single_click_activate(false);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            view,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                use gtk::gdk::{Key, ModifierType as M};
                let mods = state
                    & (M::CONTROL_MASK
                        | M::SHIFT_MASK
                        | M::ALT_MASK
                        | M::SUPER_MASK
                        | M::META_MASK);
                let primary = crate::platform::PRIMARY_MODIFIER;
                let action = match key {
                    Key::z | Key::Z if mods == primary => "win.bib-undo",
                    Key::z | Key::Z if mods == primary | M::SHIFT_MASK => "win.bib-redo",
                    Key::y | Key::Y if mods == primary => "win.bib-redo",
                    _ => return glib::Propagation::Proceed,
                };
                let _ = view.activate_action(action, None);
                glib::Propagation::Stop
            }
        ));
        view.add_controller(keys);
        root.append(
            &gtk::ScrolledWindow::builder()
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&view)
                .build(),
        );
        let message = gtk::Label::builder()
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();
        message.add_css_class("dim-label");
        root.append(&message);
        let this = Rc::new(Self {
            root,
            search,
            model,
            selection,
            view,
            indices: RefCell::default(),
            library: RefCell::default(),
            message,
        });
        this.search.connect_search_changed(glib::clone!(
            #[weak]
            this,
            move |_| this.filter()
        ));
        this
    }
    fn set(&self, result: Result<Library, String>) {
        match result {
            Ok(library) => {
                let selected = self
                    .library
                    .borrow()
                    .as_ref()
                    .map(|b| {
                        self.selected()
                            .iter()
                            .filter_map(|i| b.entries.get(*i).map(|e| e.key.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                self.selection.unselect_all();
                *self.library.borrow_mut() = Some(library);
                self.filter();
                if let Some(bib) = self.library.borrow().as_ref() {
                    for (row, i) in self.indices.borrow().iter().enumerate() {
                        if selected.contains(&bib.entries[*i].key) {
                            self.selection.select_item(row as u32, false);
                        }
                    }
                }
            }
            Err(why) => {
                *self.library.borrow_mut() = None;
                self.model.splice(0, self.model.n_items(), &[]);
                self.indices.borrow_mut().clear();
                self.message.set_text(&why);
            }
        }
    }
    fn filter(&self) {
        let library = self.library.borrow();
        let Some(bib) = library.as_ref() else {
            return;
        };
        let selected = self
            .selected()
            .into_iter()
            .filter_map(|i| bib.entries.get(i).map(|e| e.key.clone()))
            .collect::<Vec<_>>();
        let query = self.search.text().to_lowercase();
        let mut rows = Vec::new();
        let mut indices = Vec::new();
        for (i, e) in bib.entries.iter().enumerate() {
            let title = bib.value(e, "title");
            let author = bib.value(e, "author");
            let row = format!(
                "{}\n{} · {}\n{}{}",
                if title.is_empty() { &e.key } else { &title },
                author,
                bib.year(e),
                e.key,
                if bib.raw(e, "file").is_some() {
                    " · PDF"
                } else {
                    ""
                }
            );
            if query.split_whitespace().all(|q| {
                format!("{row} {}", bib.value(e, "doi"))
                    .to_lowercase()
                    .contains(q)
            }) {
                rows.push(row);
                indices.push(i);
            }
        }
        let refs = rows.iter().map(String::as_str).collect::<Vec<_>>();
        self.model.splice(0, self.model.n_items(), &refs);
        *self.indices.borrow_mut() = indices;
        for (row, &i) in self.indices.borrow().iter().enumerate() {
            if selected.contains(&bib.entries[i].key) {
                self.selection.select_item(row as u32, false);
            }
        }
        self.message.set_text(&format!(
            "{} of {} references",
            rows.len(),
            bib.entries.len()
        ));
    }
    fn selected(&self) -> Vec<usize> {
        self.indices
            .borrow()
            .iter()
            .enumerate()
            .filter(|(row, _)| self.selection.is_selected(*row as u32))
            .map(|(_, i)| *i)
            .collect()
    }
    fn select(&self, key: &str) {
        let bib = self.library.borrow();
        let Some(bib) = bib.as_ref() else {
            return;
        };
        if let Some(row) = self
            .indices
            .borrow()
            .iter()
            .position(|i| bib.entries[*i].key == key)
        {
            self.selection.select_item(row as u32, true);
        }
    }
}

pub struct Panel {
    pub root: gtk::Box,
    app: Weak<App>,
    files: gtk::StringList,
    chooser: gtk::DropDown,
    single: gtk::Label,
    entries: Rc<Entries>,
    generation: Cell<u64>,
    changing: Cell<bool>,
}
impl Panel {
    pub fn new(app: &Rc<App>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let files = gtk::StringList::new(&[]);
        let chooser = gtk::DropDown::new(Some(files.clone()), None::<gtk::Expression>);
        chooser.set_enable_search(true);
        chooser.set_tooltip_text(Some("Bibliography File"));
        let single = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::Middle)
            .xalign(0.0)
            .margin_start(12)
            .margin_end(12)
            .build();
        root.append(&chooser);
        root.append(&single);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for (icon, label, action) in [
            ("list-add-symbolic", "Add Reference…", "bib-add"),
            ("document-open-symbolic", "Open Bibliography", "bib-open"),
        ] {
            bar.append(&action_button(icon, label, action));
        }
        bar.append(&menu());
        root.append(&bar);
        let entries = Entries::new();
        root.append(&entries.root);
        let panel = Rc::new(Self {
            root,
            app: Rc::downgrade(app),
            files,
            chooser,
            single,
            entries,
            generation: Cell::new(0),
            changing: Cell::new(false),
        });
        panel.chooser.connect_selected_notify(glib::clone!(
            #[weak]
            panel,
            move |_| {
                if !panel.changing.get() {
                    panel.refresh();
                }
            }
        ));
        panel.entries.view.connect_activate(glib::clone!(
            #[weak]
            panel,
            move |_, row| {
                if let (Some(app), Some(path), Some(&i)) = (
                    panel.app.upgrade(),
                    panel.path(),
                    panel.entries.indices.borrow().get(row as usize),
                ) {
                    let key = panel
                        .entries
                        .library
                        .borrow()
                        .as_ref()
                        .and_then(|b| b.entries.get(i))
                        .map(|e| e.key.clone());
                    app.open_bibliography(&path, key);
                }
            }
        ));
        panel
    }
    pub fn path(&self) -> Option<String> {
        self.files
            .string(self.chooser.selected())
            .map(|s| s.to_string())
    }
    pub fn paths(&self) -> Vec<String> {
        (0..self.files.n_items())
            .filter_map(|i| self.files.string(i).map(|s| s.to_string()))
            .collect()
    }
    pub fn files(self: &Rc<Self>, paths: &[String]) {
        let mut paths = paths
            .iter()
            .filter(|s| is_bib(s))
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        if self.paths() == paths {
            return;
        }
        let selected = self.path();
        self.changing.set(true);
        self.files.splice(
            0,
            self.files.n_items(),
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let index = selected
            .as_ref()
            .and_then(|s| paths.iter().position(|p| p == s))
            .unwrap_or(0);
        self.chooser.set_selected(index as u32);
        self.changing.set(false);
        self.chooser.set_visible(paths.len() > 1);
        self.single.set_visible(paths.len() == 1);
        self.single
            .set_text(paths.first().map(String::as_str).unwrap_or(""));
        if let Some(app) = self.app.upgrade()
            && let Some(sidebar) = app.sidebar.get()
        {
            sidebar.set_bibliography_visible(!paths.is_empty());
        }
        self.refresh();
    }
    pub fn select(self: &Rc<Self>, path: &str) {
        let mut paths = self.paths();
        if !paths.iter().any(|p| p == path) {
            paths.push(path.into());
            self.files(&paths);
        }
        if let Some(i) = self.paths().iter().position(|p| p == path) {
            self.chooser.set_selected(i as u32);
        }
        self.refresh();
    }
    pub fn refresh(self: &Rc<Self>) {
        let (Some(app), Some(path)) = (self.app.upgrade(), self.path()) else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if let Some(tab) = app.tab_for(&path) {
            self.entries
                .set(Library::parse(&tab.text()).map_err(|e| e.to_string()));
            return;
        }
        let Some(vault) = app.vault().cloned() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = work::off_thread("read bibliography", move || {
                vault
                    .read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|(s, _)| Library::parse(&s).map_err(|e| e.to_string()))
            })
            .await;
            if let Some(panel) = weak.upgrade()
                && panel.generation.get() == generation
            {
                panel
                    .entries
                    .set(result.unwrap_or_else(|| Err("Reader stopped".into())));
            }
        });
    }
}

pub struct Editor {
    pub root: gtk::Box,
    app: Weak<App>,
    tab: Weak<Tab>,
    entries: Rc<Entries>,
    body: gtk::Box,
    source: gtk::ToggleButton,
    refresh: crate::widgets::Debounce,
}
impl Editor {
    pub fn new(app: &Rc<App>, tab: &Rc<Tab>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.add_css_class("toolbar");
        toolbar.append(&action_button(
            "list-add-symbolic",
            "Add Reference…",
            "bib-add",
        ));
        toolbar.append(&menu());
        let source = gtk::ToggleButton::builder()
            .label("Source")
            .tooltip_text("Edit BibTeX Source")
            .build();
        toolbar.append(&source);
        root.append(&toolbar);
        let entries = Entries::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&entries.root);
        body.set_vexpand(true);
        root.append(&body);
        let editor = Rc::new(Self {
            root,
            app: Rc::downgrade(app),
            tab: Rc::downgrade(tab),
            entries,
            body,
            source,
            refresh: crate::widgets::Debounce::new(Duration::from_millis(150)),
        });
        editor.source.connect_toggled(glib::clone!(
            #[weak]
            editor,
            move |button| {
                editor.body.set_visible(!button.is_active());
                if let Some(tab) = editor.tab.upgrade() {
                    tab.bibliography_source(button.is_active());
                }
            }
        ));
        tab.buffer.connect_changed(glib::clone!(
            #[weak]
            editor,
            move |_| {
                editor.refresh.call(glib::clone!(
                    #[weak]
                    editor,
                    move || editor.reload()
                ));
            }
        ));
        tab.buffer.connect_mark_set(glib::clone!(
            #[weak]
            editor,
            move |buffer, iter, mark| {
                if editor.source.is_active() && mark.name().as_deref() == Some("insert") {
                    let offset = buffer.text(&buffer.start_iter(), iter, true).len();
                    let key = editor.entries.library.borrow().as_ref().and_then(|bib| {
                        bib.entries
                            .iter()
                            .find(|e| e.span.contains(&offset))
                            .map(|e| e.key.clone())
                    });
                    if let Some(key) = key {
                        editor.entries.select(&key);
                    }
                }
            }
        ));
        editor.entries.view.connect_activate(glib::clone!(
            #[weak]
            editor,
            move |_, row| {
                let i = editor.entries.indices.borrow().get(row as usize).copied();
                if let Some(i) = i {
                    editor.edit(i);
                }
            }
        ));
        editor.reload();
        editor
    }
    fn reload(&self) {
        if let Some(tab) = self.tab.upgrade() {
            self.entries.set(
                Library::parse(&tab.text()).map_err(|e| format!("{e}. Correct this in Source.")),
            );
        }
        if let Some(app) = self.app.upgrade()
            && let Some(panel) = app.bibliography.get()
        {
            panel.refresh();
        }
    }
    pub fn key_target(&self) -> Option<gtk::Widget> {
        (!self.source.is_active()).then(|| self.entries.view.clone().upcast())
    }

    pub fn show_source(&self) {
        self.source.set_active(true);
    }
}

fn action_button(icon: &str, label: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .action_name(format!("win.{action}"))
        .build()
}
fn menu() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for (label, name) in [
        ("Edit Reference…", "edit"),
        ("Check Bibliography…", "check"),
        ("Merge Duplicates…", "merge"),
        ("Refresh Metadata…", "refresh"),
        ("Generate Citation Key…", "key"),
        ("Copy Citation Keys", "copy-keys"),
        ("Insert Citation", "cite"),
        ("Find Citation Usages…", "usages"),
        ("Open Attached PDF", "pdf"),
        ("Attach PDF…", "attach"),
        ("Download PDF…", "download"),
        ("Copy to Bibliography…", "copy"),
        ("Move to Bibliography…", "move"),
        ("Remove References…", "remove"),
        ("Undo Bibliography Edit", "undo"),
        ("Redo Bibliography Edit", "redo"),
        ("New Bibliography…", "new"),
    ] {
        menu.append(Some(label), Some(&format!("win.bib-{name}")));
    }
    gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Bibliography Actions")
        .menu_model(&menu)
        .build()
}

impl App {
    pub fn open_bibliography(self: &Rc<Self>, path: &str, key: Option<String>) {
        self.with_tab(path, Opened::Kept, "open bibliography", move |_, tab| {
            if let Some(editor) = tab.bibliography.borrow().as_ref() {
                editor.source.set_active(false);
                if let Some(key) = &key {
                    editor.entries.select(key);
                }
            }
        });
    }
    pub fn install_bibliography(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !is_bib(&tab.rel()) {
            return;
        }
        let editor = Editor::new(self, tab);
        tab.install_bibliography(&editor.root);
        *tab.bibliography.borrow_mut() = Some(editor);
        if let Some(panel) = self.bibliography.get() {
            panel.select(&tab.rel());
        }
    }
    pub fn sync_bibliography(self: &Rc<Self>) {
        if let Some(tab) = self.active()
            && is_bib(&tab.rel())
            && let Some(panel) = self.bibliography.get()
        {
            panel.select(&tab.rel());
        }
    }
    fn bibliography_target(&self) -> Option<(String, Vec<usize>)> {
        // A sidebar action operates on its selection even while a different .bib tab is open.
        let sidebar_focused = gtk::prelude::GtkWindowExt::focus(&self.window).is_some_and(|w| {
            self.sidebar_column.is_ancestor(&w) || w.is_ancestor(&self.sidebar_column)
        });
        if !sidebar_focused
            && let Some(tab) = self.active()
            && let Some(editor) = tab.bibliography.borrow().as_ref()
        {
            return Some((tab.rel(), editor.entries.selected()));
        }
        self.bibliography
            .get()
            .and_then(|panel| panel.path().map(|p| (p, panel.entries.selected())))
    }
    pub fn bibliography_action(self: &Rc<Self>, name: &str) {
        if name == "new" {
            return self.new_bibliography();
        }
        let Some((path, selected)) = self.bibliography_target() else {
            return self.new_bibliography();
        };
        match name {
            "open" => self.open_bibliography(&path, None),
            "add" => self.add_reference(&path),
            "copy-keys" | "cite" => self.cite_references(&path, selected, name == "cite"),
            _ => {
                let name = name.to_string();
                let shown = self.tab_for(&path).map(|t| t.text()).or_else(|| {
                    self.bibliography.get().and_then(|p| {
                        p.entries
                            .library
                            .borrow()
                            .as_ref()
                            .map(|b| b.source.clone())
                    })
                });
                self.with_tab(&path, Opened::Kept, "edit bibliography", move |app, tab| {
                    if shown.as_ref().is_some_and(|text| *text != tab.text()) {
                        return app.toast("Bibliography changed; select the references again");
                    }
                    let editor = tab.bibliography.borrow().clone();
                    if let Some(editor) = editor {
                        editor.operate(app, &name, selected);
                    }
                });
            }
        }
    }
}
