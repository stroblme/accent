//! The bibliography sidebar edits ordinary source tabs through their save gate and undo history.
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

/// Virtualized reference rows, styled like the Search pane.
struct Entries {
    root: gtk::Box,
    search: gtk::SearchEntry,
    model: gio::ListStore,
    selection: gtk::MultiSelection,
    view: gtk::ListView,
    indices: RefCell<Vec<usize>>,
    library: RefCell<Option<Library>>,
    message: gtk::Label,
}
impl Entries {
    fn new(app: &Rc<App>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search References")
            .hexpand(true)
            .width_chars(1)
            .build();
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let weak = Rc::downgrade(app);
        let factory = crate::widgets::factory(
            move |item| {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                row.set_margin_top(6);
                row.set_margin_bottom(6);
                let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
                text.set_hexpand(true);
                for classes in [&["heading"][..], &["dim-label"], &["caption", "dim-label"]] {
                    let label = gtk::Label::builder()
                        .xalign(0.0)
                        .ellipsize(pango::EllipsizeMode::End)
                        .css_classes(classes)
                        .build();
                    text.append(&label);
                }
                row.append(&text);
                let remove = crate::widgets::icon_button("user-trash-symbolic", "Remove Reference");
                let weak = weak.clone();
                remove.connect_clicked(glib::clone!(
                    #[weak]
                    item,
                    move |_| {
                        if let Some(app) = weak.upgrade()
                            && let Some(panel) = app.bibliography.get()
                        {
                            panel.remove(item.position());
                        }
                    }
                ));
                let actions = crate::widgets::hover_revealer();
                actions.set_valign(gtk::Align::Center);
                actions.set_child(Some(&remove));
                row.append(&actions);
                row
            },
            |row: &gtk::Box, item: &gtk::ListItem| {
                // As in Git, wire the GTK list row after the list manager finishes binding it.
                let list_row = row.parent();
                glib::idle_add_local_once(move || {
                    if let Some(row) = list_row {
                        crate::widgets::reveal_on_hover(&row);
                    }
                });
                let Some(value) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                    return;
                };
                let fields = value.borrow::<[String; 3]>();
                let mut child = row.first_child().and_then(|text| text.first_child());
                for text in fields.iter() {
                    let Some(label) = child.and_downcast::<gtk::Label>() else {
                        return;
                    };
                    label.set_text(text);
                    label.set_tooltip_text(Some(text));
                    child = label.next_sibling();
                }
            },
        );
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.add_css_class("navigation-sidebar");
        view.set_single_click_activate(true);
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
        message.set_css_classes(&["caption", "dim-label", "numeric"]);
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
                self.model.remove_all();
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
            let row = [
                if title.is_empty() {
                    e.key.clone()
                } else {
                    title
                },
                format!("{} · {}", author, bib.year(e)),
                format!(
                    "{}{}",
                    e.key,
                    if bib.raw(e, "file").is_some() {
                        " · PDF"
                    } else {
                        ""
                    }
                ),
            ];
            if query.split_whitespace().all(|q| {
                format!("{} {}", row.join(" "), bib.value(e, "doi"))
                    .to_lowercase()
                    .contains(q)
            }) {
                rows.push(glib::BoxedAnyObject::new(row));
                indices.push(i);
            }
        }
        self.model.splice(0, self.model.n_items(), &rows);
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
    changed: crate::widgets::Debounce,
}
impl Panel {
    pub fn new(app: &Rc<App>) -> Rc<Self> {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let files = gtk::StringList::new(&[]);
        let chooser = gtk::DropDown::builder()
            .model(&files)
            .hexpand(true)
            .enable_search(true)
            // The search filters by this expression and does nothing without one; any part of
            // the path matches, its folders coming before the name.
            .expression(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<&gtk::Expression>,
                "string",
            ))
            .search_match_mode(gtk::StringFilterMatchMode::Substring)
            .tooltip_text("Bibliography File")
            .factory(&crate::widgets::label_factory(
                pango::EllipsizeMode::End,
                |label, item| {
                    // Cap the natural request, not the allocated width: the label fills the pane.
                    label.set_max_width_chars(1);
                    if let Some(name) = item.item().and_downcast::<gtk::StringObject>() {
                        label.set_text(&name.string());
                        label.set_tooltip_text(Some(&name.string()));
                    }
                },
            ))
            .list_factory(&crate::widgets::label_factory(
                pango::EllipsizeMode::None,
                |label, item| {
                    if let Some(name) = item.item().and_downcast::<gtk::StringObject>() {
                        label.set_text(&name.string());
                    }
                },
            ))
            .build();
        let single = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::Middle)
            .max_width_chars(1)
            .hexpand(true)
            .xalign(0.0)
            .margin_start(6)
            .margin_end(6)
            .build();
        root.append(&chooser);
        root.append(&single);
        let entries = Entries::new(app);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&entries.search);
        let open = action_button("filetype-file-symbolic", "Open Bibliography", "bib-open");
        open.add_css_class("flat");
        bar.append(&open);
        let more = menu();
        more.add_css_class("flat");
        bar.append(&more);
        let add = action_button("list-add-symbolic", "Add Reference…", "bib-add");
        add.add_css_class("suggested-action");
        bar.append(&add);
        root.append(&bar);
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
            changed: crate::widgets::Debounce::new(Duration::from_millis(150)),
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
                let key = panel
                    .entries
                    .indices
                    .borrow()
                    .get(row as usize)
                    .and_then(|&i| {
                        panel
                            .entries
                            .library
                            .borrow()
                            .as_ref()
                            .and_then(|b| b.entries.get(i))
                            .map(|e| e.key.clone())
                    });
                if let (Some(app), Some(path), Some(key)) = (panel.app.upgrade(), panel.path(), key)
                {
                    app.open_bibliography(&path, Some(key));
                }
            }
        ));
        panel
    }
    fn remove(self: &Rc<Self>, row: u32) {
        let (Some(app), Some(path)) = (self.app.upgrade(), self.path()) else {
            return;
        };
        let Some(index) = self.entries.indices.borrow().get(row as usize).copied() else {
            return;
        };
        let Some(before) = self
            .entries
            .library
            .borrow()
            .as_ref()
            .map(|bib| bib.source.clone())
        else {
            return;
        };
        app.with_tab(&path, Opened::Kept, "remove reference", move |app, tab| {
            if tab.text() != before {
                return app.toast("Bibliography changed; select the reference again");
            }
            edit::operate(app, tab, "remove", vec![index]);
        });
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
        self.single
            .set_tooltip_text(paths.first().map(String::as_str));
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

fn action_button(icon: &str, label: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .action_name(format!("win.{action}"))
        .valign(gtk::Align::Center)
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
        if let Some(key) = key {
            self.with_tab(path, Opened::Preview, "open bibliography", move |_, tab| {
                if let Ok(bib) = Library::parse(&tab.text())
                    && let Some(entry) = bib.entries.iter().find(|e| e.key == key)
                {
                    let at = tab
                        .buffer
                        .iter_at_offset(bib.source[..entry.span.start].chars().count() as i32);
                    tab.buffer.place_cursor(&at);
                    tab.show_at(&at, 0.25);
                }
            });
        } else {
            self.open_path(path);
        }
    }

    pub fn watch_bibliography(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !is_bib(&tab.rel()) {
            return;
        }
        let Some(panel) = self.bibliography.get() else {
            return;
        };
        panel.select(&tab.rel());
        tab.buffer.connect_changed(glib::clone!(
            #[weak]
            panel,
            #[weak]
            tab,
            move |_| {
                if panel.path().as_deref() == Some(tab.rel().as_str()) {
                    panel.changed.call(glib::clone!(
                        #[weak]
                        panel,
                        move || panel.refresh()
                    ));
                }
            }
        ));
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
            && is_bib(&tab.rel())
        {
            let bib = Library::parse(&tab.text()).ok();
            let at = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
            let offset = tab.buffer.text(&tab.buffer.start_iter(), &at, true).len();
            let selected = bib
                .as_ref()
                .and_then(|b| b.entries.iter().position(|e| e.span.contains(&offset)))
                .into_iter()
                .collect();
            return Some((tab.rel(), selected));
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
                    edit::operate(app, tab, &name, selected);
                });
            }
        }
    }
}
