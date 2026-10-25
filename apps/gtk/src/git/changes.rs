//! The changed-files list: the rows one `git status` becomes, and the widgets they are drawn in.

use super::*;
use accent_api::git::Sides;
use std::collections::BTreeMap;

/// How far one level of the changes tree is indented, in px. `GtkTreeExpander`'s own step, so a
/// folder here sits where the same folder sits in the Files tree.
const INDENT: i32 = 16;

/// How far a file row is inset past the chevron its folder row leads with, in px: the chevron and
/// the box's spacing. Without it a file starts under its folder's icon rather than under its name,
/// and the two read as one column of rows rather than as a tree.
const FILE_INSET: i32 = 22;

/// Which list a row belongs to, which is what decides the letter it shows, the buttons it offers
/// and what activating it compares.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Section {
    Conflicts,
    Staged,
    Changes,
}

/// One line of the changes list. Headers are rows of their own rather than list sections, so the
/// whole thing is one flat `ListStore` and an empty section is simply two rows that are not there.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Row {
    Header {
        title: &'static str,
        /// The section a "Stage All" / "Unstage All" button acts on, or `None` for a header with
        /// no bulk action: conflicts are resolved one file at a time, and submodules are a
        /// read-only list.
        all: Option<Section>,
        /// Whether the header offers Discard All: the Changes header's alone, and only where
        /// Discard can take every entry of the section, as a folder row's ([`discardable`]).
        discardable: bool,
    },
    /// A folder in the tree view, standing for everything under it in one section.
    Folder {
        /// The whole path from the repository root, which is what identifies the row while it is
        /// collapsed.
        path: String,
        /// What the row shows: the segments this row adds to the one above it. A chain of folders
        /// with a single child each lands on one row, so this is a path rather than a name.
        label: String,
        section: Section,
        depth: usize,
        /// Whether what is under it is listed, which is what its chevron says.
        open: bool,
        /// Whether Discard can take every entry under it ([`discardable`]).
        discardable: bool,
    },
    Entry {
        entry: Entry,
        section: Section,
        /// The path as the rest of the app names it: vault-relative, or absolute outside it.
        key: String,
        /// How far the row is indented. Always 0 in the flat view.
        depth: usize,
    },
    Submodule(Submodule),
}

impl Panel {
    pub(super) fn wire_changes(self: &Rc<Self>, view: &gtk::ListView) {
        let bound = Rc::downgrade(self);
        view.set_factory(Some(&crate::widgets::factory(
            |_| row_stack(),
            // The row is rebuilt from the item rather than from the stack handed over: a
            // recycled row draws what it is bound to, not what it held.
            move |_: &gtk::Stack, item| bind_change(item, &bound),
        )));

        // A press over the list holds its rows still until the release, which is then answered
        // with the redraw that was held off. Comparing first keeps the rows a refresh does not
        // touch, but a Stage elsewhere still replaces the rows between the two sections it moves
        // a file across, and a press on one of those lost its click (XTEST, 2026-09-10). Capture
        // phase, so the release is seen on its way to the button, and the redraw waits an idle
        // so that the button has had it first.
        let held = gtk::EventControllerLegacy::new();
        held.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        held.connect_event(move |_, event| {
            use gdk::EventType as E;
            let Some(panel) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                E::ButtonPress | E::TouchBegin => panel.pressed.set(true),
                E::ButtonRelease | E::TouchEnd | E::TouchCancel | E::GrabBroken => {
                    panel.pressed.set(false);
                    let weak = Rc::downgrade(&panel);
                    glib::idle_add_local_once(move || {
                        if let Some(panel) = weak.upgrade() {
                            panel.rebuild_changes();
                        }
                    });
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        view.add_controller(held);

        let weak = Rc::downgrade(self);
        view.connect_activate(move |view, position| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            let item = view.model().and_then(|m| m.item(position));
            if let Some(row) = boxed::<Row>(item.clone()) {
                if let (Row::Folder { path, open, .. }, Some(item)) = (&row, item) {
                    turn(view, item, path, !open);
                }
                panel.activate(&row);
            }
        });
    }

    /// Rebuild from cached status for grouping and folder toggles, without another git query.
    /// Splice only [`changed_runs`] through [`Fill`] so unchanged rows retain focus and
    /// press/release handling.
    ///
    /// Rows must carry all binding data; [`Panel::set_tree`] resets the list for the view mode
    /// they do not carry. Defer fills while a press is held, then redraw on release.
    pub(super) fn rebuild_changes(&self) {
        if self.pressed.get() {
            return;
        }
        let rows = {
            let state = self.state.borrow();
            match (
                state.statuses.get(state.selected),
                state.repos.get(state.selected),
            ) {
                (Some(status), Some(repo)) => rows_of(
                    status,
                    &state.submodules,
                    &|path| vault_key(&self.source().key_root(), repo, path),
                    self.tree.get(),
                    &self.collapsed.borrow(),
                ),
                _ => Vec::new(),
            }
        };
        let runs = changed_runs(&self.changes, &rows);
        if runs.is_empty() {
            return;
        }
        let pressed = self.pressed.clone();
        self.changes_fill
            .splice(&self.changes, runs, move || pressed.get(), || {});
    }

    /// Activate the row `path` is listed on, as a click on it does, and say which section it was
    /// in. `ACCENT_BENCH_COMPARE=row:` and nothing else: the headless image has no pointer.
    #[cfg(feature = "bench")]
    pub fn activate_change(self: &Rc<Self>, path: &str) -> Option<&'static str> {
        let row = (0..self.changes.n_items())
            .filter_map(|i| boxed::<Row>(self.changes.item(i)))
            .find(|row| matches!(row, Row::Entry { entry, .. } if entry.path == path))?;
        let Row::Entry { section, .. } = &row else {
            return None;
        };
        let section = *section;
        self.activate(&row);
        Some(match section {
            Section::Conflicts => "conflicts",
            Section::Staged => "staged",
            Section::Changes => "changes",
        })
    }

    fn activate(self: &Rc<Self>, row: &Row) {
        if let Row::Folder { path, section, .. } = row {
            let key = folder_key(*section, path);
            {
                let mut collapsed = self.collapsed.borrow_mut();
                if !collapsed.remove(&key) {
                    collapsed.insert(key);
                }
            }
            return self.rebuild_changes();
        }
        let Row::Entry {
            entry,
            section,
            key,
            ..
        } = row
        else {
            return;
        };
        match sides_for(*section, entry) {
            Some(sides) => self.compare(&entry.path, key, sides),
            None => self.open_merge(&entry.path, key),
        }
    }
}

/// Update the focused folder row's item and chevron in place before splicing its children.
/// Replacing that row would lose keyboard focus and its hover/press state. Rows without focus
/// follow normal replacement.
fn turn(view: &gtk::ListView, item: glib::Object, path: &str, open: bool) {
    let (focus, view) = (view.root().and_then(|root| root.focus()), view.upcast_ref());
    let stack = focus
        .and_then(|f| {
            std::iter::successors(Some(f), |w| w.parent())
                .find(|w| w.parent().as_ref() == Some(view))
        })
        .and_then(|row| row.first_child())
        .and_downcast::<gtk::Stack>()
        .filter(|s| {
            s.visible_child_name().as_deref() == Some("folder")
                && s.tooltip_text().as_deref() == Some(path)
        });
    let (Some(stack), Ok(item)) = (stack, item.downcast::<glib::BoxedAnyObject>()) else {
        return;
    };
    let chevron = stack
        .visible_child()
        .and_then(|folder| folder.first_child());
    if let Some(chevron) = chevron.and_downcast::<gtk::Image>() {
        chevron.set_icon_name(Some(chevron_icon(open)));
    }
    if let Row::Folder { open: shown, .. } = &mut *item.borrow_mut::<Row>() {
        *shown = open;
    }
}

/// A folder row's chevron: the fold chevrons' pair rather than `pan-*`, for the reason the branch
/// button gives.
fn chevron_icon(open: bool) -> &'static str {
    match open {
        true => "go-down-symbolic",
        false => "go-next-symbolic",
    }
}

/// What activating a changed file compares, or `None` for a conflict, which opens as a merge of
/// the file between its two sides rather than as a diff of two texts. A file deleted from the
/// working tree has no tab to compare inside, so it gets one of its own: the index against
/// nothing.
fn sides_for(section: Section, entry: &Entry) -> Option<Sides> {
    match section {
        Section::Conflicts => None,
        Section::Staged => Some(Sides::Staged {
            orig: entry.orig.clone(),
        }),
        Section::Changes if entry.y == 'D' => Some(Sides::Deleted),
        Section::Changes => Some(Sides::Worktree),
    }
}

/// A changes row's section header, the first of the three layouts a row's stack can show — a
/// header, a folder or an entry, as the row bound to it is ([`layout`]): its title, then Stage All
/// or Unstage All and Discard All. The buttons of all three hold the `GtkListItem` rather than the
/// row's data, because the data is replaced under them every time the row is reused.
fn header_layout(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Box {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    // Stage All or Unstage All, as the binder says: the +/− a row's own buttons use.
    let all = icon_button("list-add-symbolic", "Stage All");
    let weak = panel.clone();
    all.connect_clicked(glib::clone!(
        #[weak]
        item,
        move |_| {
            let (
                Some(panel),
                Some(Row::Header {
                    all: Some(section), ..
                }),
            ) = (weak.upgrade(), boxed(item.item()))
            else {
                return;
            };
            let paths = panel.section_paths(section, "");
            match section {
                Section::Staged => panel.unstage(paths),
                _ => panel.stage(paths),
            }
        }
    ));
    // Discard All beside it, over the rows' own Discard: every change in the section, after the
    // one question a folder row's Discard asks.
    let discard = icon_button("document-revert-symbolic", "Discard All");
    let weak = panel.clone();
    discard.connect_clicked(move |_| {
        if let Some(panel) = weak.upgrade() {
            panel.discard(Some(""), panel.section_entries(Section::Changes, ""));
        }
    });
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&all);
    header.append(&discard);
    header
}

/// A folder of the tree view: the chevron says whether it is open, the folder icon says it is
/// one — the same icon the Files tree gives a directory — and the label carries whatever segments
/// this row adds to the one above it.
fn folder_layout(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Box {
    let chevron = gtk::Image::new();
    chevron.add_css_class("dim-label");
    let folder_name = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    let folder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    folder.append(&chevron);
    folder.append(&gtk::Image::from_icon_name(crate::doc::FOLDER_ICON));
    folder.append(&folder_name);
    folder.append(&actions(item, panel));
    folder
}

/// A changed file, or a submodule.
fn entry_layout(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Box {
    let entry = file_line();
    entry.append(&actions(item, panel));
    entry
}

/// Which of a row's three buttons was pressed.
#[derive(Clone, Copy)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

/// Stage, Unstage and Discard buttons for file and folder rows, filtered by [`fit`]. Reveal on
/// hover or focus without reserving hidden width, and build lazily through [`revealed_actions`]
/// to reduce row widgets.
fn actions(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Revealer {
    let panel = panel.clone();
    revealed_actions(item, move |item| {
        let buttons = action_buttons(item, &panel);
        if let Some(row) = boxed::<Row>(item.item()) {
            fit(&buttons, &row);
        }
        buttons
    })
}

/// The three buttons of [`actions()`], each acting on whatever row `item` holds when it is pressed.
fn action_buttons(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Box {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for (icon, tooltip, act) in [
        ("list-add-symbolic", "Stage", Act::Stage),
        ("list-remove-symbolic", "Unstage", Act::Unstage),
        ("document-revert-symbolic", "Discard", Act::Discard),
    ] {
        let button = icon_button(icon, tooltip);
        let weak = panel.clone();
        button.connect_clicked(glib::clone!(
            #[weak]
            item,
            move |_| {
                if let (Some(panel), Some(row)) = (weak.upgrade(), boxed::<Row>(item.item())) {
                    panel.act(act, row);
                }
            }
        ));
        actions.append(&button);
    }
    actions
}

/// The buttons inside an [`actions()`] revealer, once it has revealed them.
fn buttons(revealer: Option<gtk::Widget>) -> Option<gtk::Box> {
    revealer
        .and_downcast::<gtk::Revealer>()?
        .child()
        .and_downcast::<gtk::Box>()
}

impl Panel {
    /// What one of a row's buttons does. A folder's buttons act on every entry of its section
    /// under it, picked the way the section header's Stage All and Unstage All pick theirs.
    fn act(self: &Rc<Self>, act: Act, row: Row) {
        let (entries, folder) = match &row {
            Row::Entry { entry, .. } => (vec![entry.clone()], None),
            Row::Folder { path, section, .. } => (
                self.section_entries(*section, &format!("{path}/")),
                Some(path.as_str()),
            ),
            _ => return,
        };
        match act {
            Act::Stage => self.stage(entries.into_iter().map(|e| e.path).collect()),
            Act::Unstage => self.unstage(entries.into_iter().map(|e| e.path).collect()),
            Act::Discard => self.discard(folder, entries),
        }
    }
}

fn bind_change(item: &gtk::ListItem, weak: &Weak<Panel>) {
    let (Some(stack), Some(row), Some(panel)) = (
        item.child().and_downcast::<gtk::Stack>(),
        boxed::<Row>(item.item()),
        weak.upgrade(),
    ) else {
        return;
    };
    // The list row itself, which is not there at setup — `set_child` only stores the widget and
    // the row it goes in is made when the item is first bound — and must not be touched from
    // inside the bind either: a class added or a handler connected on it there left the list
    // manager handing GTK null children, a `gtk_widget_insert_after: assertion 'GTK_IS_WIDGET'`
    // per row. An idle is past the manager's pass and costs one closure per bind.
    let list_row = stack.parent();
    let (menu_item, menu_panel) = (item.downgrade(), weak.clone());
    glib::idle_add_local_once(move || {
        if let Some(list_row) = list_row {
            reveal_on_hover(&list_row);
            // The list item is this row's for good, whatever it is bound to next.
            crate::widgets::row_menu(&list_row, move |stack| {
                change_menu(stack, &menu_item.upgrade()?, &menu_panel)
            });
        }
    });
    // A header only titles its section and activating it does nothing, so it takes no hover
    // highlight: the row lit up around its own button read as one control. A recycled row may
    // have been a header, which is why every other row sets it back.
    item.set_activatable(!matches!(row, Row::Header { .. }));

    match row {
        Row::Header {
            title: text,
            all: section,
            discardable,
        } => {
            let header = layout(&stack, "header", || header_layout(item, weak));
            let Some(title) = header.first_child().and_downcast::<gtk::Label>() else {
                return;
            };
            let (Some(all), Some(discard_all)) = (
                title.next_sibling().and_downcast::<gtk::Button>(),
                header.last_child(),
            ) else {
                return;
            };
            stack.set_tooltip_text(None);
            title.set_text(text);
            all.set_visible(section.is_some());
            discard_all.set_visible(discardable);
            let (icon, tip) = match section {
                Some(Section::Staged) => ("list-remove-symbolic", "Unstage All"),
                _ => ("list-add-symbolic", "Stage All"),
            };
            all.set_icon_name(icon);
            all.set_tooltip_text(Some(tip));
            all.update_property(&[gtk::accessible::Property::Label(tip)]);
        }
        Row::Folder {
            label,
            depth,
            path,
            open,
            ..
        } => {
            let folder = layout(&stack, "folder", || folder_layout(item, weak));
            let Some(chevron) = folder.first_child().and_downcast::<gtk::Image>() else {
                return;
            };
            let Some(text) = folder
                .last_child()
                .and_then(|revealer| revealer.prev_sibling())
                .and_downcast::<gtk::Label>()
            else {
                return;
            };
            chevron.set_icon_name(Some(chevron_icon(open)));
            text.set_text(&label);
            folder.set_margin_start(depth as i32 * INDENT);
            stack.set_tooltip_text(Some(&path));
        }
        Row::Entry {
            entry: e,
            section,
            depth,
            ..
        } => {
            let entry = layout(&stack, "entry", || entry_layout(item, weak));
            entry.set_margin_start(inset(depth, panel.tree.get()));
            let directory = match depth {
                0 => split_name(&e.path).0,
                _ => "",
            };
            let icon = crate::doc::icon_for(&e.path);
            bind_file_line(&entry, icon, status_letter(&e, section), &e.path, directory);
            // A repository of its own offers nothing, and says so where its path would be: the
            // chooser lists it under its own name.
            let own = own_repository(&e.path);
            let tip = match own {
                true => {
                    let name = split_name(e.path.trim_end_matches('/')).1;
                    format!("{name} is a repository of its own")
                }
                false => e.path.clone(),
            };
            stack.set_tooltip_text(Some(&tip));
        }
        Row::Submodule(sub) => {
            let entry = layout(&stack, "entry", || entry_layout(item, weak));
            entry.set_margin_start(0);
            let directory = sub
                .describe
                .as_deref()
                .unwrap_or_else(|| split_name(&sub.path).0);
            bind_file_line(
                &entry,
                crate::doc::FOLDER_ICON,
                sub.state,
                &sub.path,
                directory,
            );
            stack.set_tooltip_text(Some(&sub.oid));
        }
    }
    // A row that has revealed its buttons before keeps them, fitted to every row bound to it since.
    let shown = stack.visible_child().and_then(|layout| layout.last_child());
    if let (Some(buttons), Some(row)) = (buttons(shown), boxed::<Row>(item.item())) {
        fit(&buttons, &row);
    }
}

/// Which of a row's [`actions()`] it shows ([`offered`]).
fn fit(buttons: &gtk::Box, row: &Row) {
    let offered = offered(row);
    buttons.set_visible(offered.is_some());
    let (Some(stage), Some(discard)) = (buttons.first_child(), buttons.last_child()) else {
        return;
    };
    if let (Some(unstage), Some(on)) = (stage.next_sibling(), offered) {
        stage.set_visible(on[0]);
        unstage.set_visible(on[1]);
        discard.set_visible(on[2]);
    }
}

/// Which of Stage, Unstage and Discard a row offers, or `None` for no buttons at all: on a
/// folder, none in Merge Conflicts, for the reason its header has no Stage All — a conflict is
/// resolved one file at a time; on a file, none for a repository of its own; on a submodule none.
/// Staging a conflicted file is how git is told it is resolved, so Stage is there for it too;
/// unstaging one is not a thing the pane offers. Discard is for the working tree alone, and only
/// where everything the row stands for can go ([`discardable`]).
fn offered(row: &Row) -> Option<[bool; 3]> {
    let (section, discardable) = match row {
        Row::Folder {
            section,
            discardable,
            ..
        } if *section != Section::Conflicts => (*section, *discardable),
        Row::Entry {
            entry,
            section,
            key,
            ..
        } if !own_repository(&entry.path) => (*section, discardable(entry, key)),
        _ => return None,
    };
    Some([
        section != Section::Staged,
        section == Section::Staged,
        section == Section::Changes && discardable,
    ])
}

/// A file or folder row's right-click menu: Open, which a click on a file does, then the row's
/// buttons, through a `change` action group put on the layout the row's stack shows, which the
/// menu hangs from.
fn change_menu(
    stack: &gtk::Widget,
    item: &gtk::ListItem,
    panel: &Weak<Panel>,
) -> Option<(gtk::Widget, gio::Menu)> {
    let row = boxed::<Row>(item.item())?;
    let host = stack
        .clone()
        .downcast::<gtk::Stack>()
        .ok()?
        .visible_child()?;
    let on = offered(&row).unwrap_or_default();
    let file = matches!(row, Row::Entry { .. });
    let group = gio::SimpleActionGroup::new();
    let menu = gio::Menu::new();
    let mut section = gio::Menu::new();
    for (label, name, shown, act) in [
        ("Open", "open", file, None),
        ("Stage", "stage", on[0], Some(Act::Stage)),
        ("Unstage", "unstage", on[1], Some(Act::Unstage)),
        ("Discard", "discard", on[2], Some(Act::Discard)),
    ] {
        if act.is_some() && name == "stage" && section.n_items() > 0 {
            menu.append_section(None, &section);
            section = gio::Menu::new();
        }
        if !shown {
            continue;
        }
        let action = gio::SimpleAction::new(name, None);
        let (row, panel) = (row.clone(), panel.clone());
        action.connect_activate(move |_, _| {
            if let Some(panel) = panel.upgrade() {
                match act {
                    Some(act) => panel.act(act, row.clone()),
                    None => panel.activate(&row),
                }
            }
        });
        group.add_action(&action);
        section.append(Some(label), Some(&format!("change.{name}")));
    }
    if section.n_items() > 0 {
        menu.append_section(None, &section);
    }
    if menu.n_items() == 0 {
        return None;
    }
    host.insert_action_group("change", Some(&group));
    Some((host, menu))
}

/// Whether Discard can restore `entry`. Untracked files outside the vault are unsupported
/// because the trash hook accepts vault keys; folders containing them also offer no Discard.
fn discardable(entry: &Entry, key: &str) -> bool {
    !own_repository(&entry.path) && (entry.x != '?' || !Path::new(key).is_absolute())
}

/// Whether a folder's or a section's Discard has something to take and can take all of it. A
/// repository of its own under it is skipped, not counted against it ([`own_repository`]).
fn discards(entries: &[&Entry], key: &dyn Fn(&str) -> String) -> bool {
    let mut taken = entries
        .iter()
        .filter(|e| !own_repository(&e.path))
        .peekable();
    taken.peek().is_some() && taken.all(|e| discardable(e, &key(&e.path)))
}

/// The changes list: the four sections in order, each behind a header, empty ones dropped.
///
/// `key` turns a repository-relative path into the key the rest of the app uses; the tests pass
/// identity, and the pane passes [`vault_key`] bound to the selected repository. `tree` groups each
/// section's files by folder, and `collapsed` holds the [`folder_key`]s whose contents are hidden.
fn rows_of(
    status: &Status,
    subs: &[Submodule],
    key: &dyn Fn(&str) -> String,
    tree: bool,
    collapsed: &HashSet<String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let sections = [
        ("Merge Conflicts", Section::Conflicts, None),
        ("Staged Changes", Section::Staged, Some(Section::Staged)),
        ("Changes", Section::Changes, Some(Section::Changes)),
    ];
    for (title, section, all) in sections {
        let entries: Vec<&Entry> = match section {
            Section::Conflicts => status.conflicts().collect(),
            Section::Staged => status.staged().collect(),
            Section::Changes => status.changes().collect(),
        };
        if entries.is_empty() {
            continue;
        }
        let discardable = section == Section::Changes && discards(&entries, key);
        rows.push(Row::Header {
            title,
            all,
            discardable,
        });
        match tree {
            true => rows.extend(grouped(&entries, section, collapsed, key)),
            false => rows.extend(entries.into_iter().map(|entry| Row::Entry {
                key: key(&entry.path),
                entry: entry.clone(),
                section,
                depth: 0,
            })),
        }
    }
    if !subs.is_empty() {
        rows.push(Row::Header {
            title: "Submodules",
            all: None,
            discardable: false,
        });
        rows.extend(subs.iter().cloned().map(Row::Submodule));
    }
    rows
}

/// How far in a file row starts. In the folder view every file clears the chevron, the root's
/// included, so its icon lands under a sibling folder's; flat rows are not indented at all, and
/// share depth 0 with the root's files, which is why the view is asked rather than the depth.
fn inset(depth: usize, tree: bool) -> i32 {
    match tree {
        true => depth as i32 * INDENT + FILE_INSET,
        false => 0,
    }
}

/// What identifies a folder row while it is collapsed. The section is part of it because the same
/// folder can have a row under Staged and another under Changes, and folding one is not folding
/// the other.
fn folder_key(section: Section, dir: &str) -> String {
    format!("{section:?}/{dir}")
}

/// Group one section by folder, sorting folders before files at each level. Compress
/// single-child chains into one path row and omit collapsed contents, keeping the tree compact.
fn grouped(
    entries: &[&Entry],
    section: Section,
    collapsed: &HashSet<String>,
    key: &dyn Fn(&str) -> String,
) -> Vec<Row> {
    let mut rows = Vec::new();
    group_level(&mut rows, entries, "", 0, section, collapsed, key);
    rows
}

fn group_level(
    rows: &mut Vec<Row>,
    entries: &[&Entry],
    prefix: &str,
    depth: usize,
    section: Section,
    collapsed: &HashSet<String>,
    key: &dyn Fn(&str) -> String,
) {
    // A map rather than a scan per entry: an untracked tree lists every file in it, and 40 000
    // files across 800 folders took 90 ms to group by looking each folder up in a list.
    let mut dirs: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    let mut files: Vec<&Entry> = Vec::new();
    for &entry in entries {
        match segment(&entry.path, prefix) {
            Some(head) => dirs.entry(head).or_default().push(entry),
            None => files.push(entry),
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    for (name, group) in dirs {
        let mut label = name.to_string();
        while let Some(only) = only_segment(&group, &format!("{prefix}{label}/")) {
            label = format!("{label}/{only}");
        }
        let path = format!("{prefix}{label}");
        let open = !collapsed.contains(&folder_key(section, &path));
        rows.push(Row::Folder {
            label,
            section,
            depth,
            path: path.clone(),
            open,
            discardable: discards(&group, key),
        });
        if open {
            let under = format!("{path}/");
            group_level(rows, &group, &under, depth + 1, section, collapsed, key);
        }
    }
    rows.extend(files.into_iter().map(|entry| Row::Entry {
        key: key(&entry.path),
        entry: entry.clone(),
        section,
        depth,
    }));
}

/// The folder `path` lies in directly under `prefix`, or `None` where it names a file of that
/// folder. git reports an untracked nested repository as one entry ending in `/`, and that is a
/// row in its own right rather than a folder with nothing inside it.
fn segment<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    match path.get(prefix.len()..)?.split_once('/') {
        Some((head, rest)) if !rest.is_empty() => Some(head),
        _ => None,
    }
}

/// The one folder every entry of `group` lies under, or `None` where they part ways or any of them
/// is a file at this level. What decides whether a chain of folders is compressed onto one row.
fn only_segment(group: &[&Entry], prefix: &str) -> Option<String> {
    let mut heads = group.iter().map(|entry| segment(&entry.path, prefix));
    let first = heads.next()??;
    heads
        .all(|head| head == Some(first))
        .then(|| first.to_string())
}

/// The one letter a row shows: the side of porcelain's two that the section is about.
fn status_letter(e: &Entry, section: Section) -> char {
    match section {
        // Untracked is `?` in porcelain and `U` on screen, which is the letter every git UI uses
        // for it and the one a user reads as "untracked" rather than as a question.
        Section::Changes if e.x == '?' => 'U',
        Section::Changes => e.y,
        Section::Conflicts | Section::Staged => e.x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, x: char, y: char) -> Entry {
        Entry {
            path: path.to_string(),
            orig: None,
            x,
            y,
            unmerged: false,
            submodule: false,
        }
    }

    fn identity(path: &str) -> String {
        path.to_string()
    }

    #[test]
    fn rows_of_drops_the_sections_with_nothing_in_them() {
        let status = Status {
            entries: vec![entry("a.md", 'M', '.'), entry("new.md", '?', '?')],
            ..Status::default()
        };
        let rows = rows_of(&status, &[], &identity, false, &HashSet::new());
        let titles: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Header { title, .. } => Some(*title),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            ["Staged Changes", "Changes"],
            "no conflicts section"
        );
        assert_eq!(rows.len(), 4);
        assert!(
            matches!(
                &rows[2],
                Row::Header {
                    title: "Changes",
                    discardable: true,
                    ..
                }
            ),
            "Discard All over the working tree's changes, and over nothing else"
        );
        assert!(matches!(
            &rows[0],
            Row::Header {
                discardable: false,
                ..
            }
        ));
        assert!(matches!(
            &rows[1],
            Row::Entry { entry, section: Section::Staged, key, .. } if entry.path == "a.md" && key == "a.md"
        ));
    }

    #[test]
    fn rows_of_leads_with_conflicts_and_ends_with_submodules() {
        let mut conflict = entry("c.md", 'U', 'U');
        conflict.unmerged = true;
        let status = Status {
            entries: vec![conflict],
            ..Status::default()
        };
        let subs = [Submodule {
            path: "vendor/x".to_string(),
            oid: "abc".to_string(),
            state: ' ',
            describe: None,
        }];
        let rows = rows_of(&status, &subs, &identity, false, &HashSet::new());
        assert!(matches!(
            rows.first(),
            Some(Row::Header {
                title: "Merge Conflicts",
                all: None,
                discardable: false,
            })
        ));
        assert!(matches!(rows.last(), Some(Row::Submodule(_))));
    }

    /// The tree rows as `(depth, what the row shows)`, which is what the shape of the list is.
    fn shape(rows: &[Row]) -> Vec<(usize, String)> {
        rows.iter()
            .map(|row| match row {
                Row::Folder { label, depth, .. } => (*depth, label.clone()),
                Row::Entry { entry, depth, .. } => (*depth, entry.path.clone()),
                _ => (0, String::new()),
            })
            .collect()
    }

    fn entries(paths: &[&str]) -> Vec<Entry> {
        paths.iter().map(|p| entry(p, '.', 'M')).collect()
    }

    #[test]
    fn grouped_puts_folders_before_files_and_indents_what_is_under_them() {
        let held = entries(&["a.md", "src/x.md", "src/deep/y.md", "b.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(
            shape(&rows),
            [
                (0, "src".to_string()),
                (1, "deep".to_string()),
                (2, "src/deep/y.md".to_string()),
                (1, "src/x.md".to_string()),
                (0, "a.md".to_string()),
                (0, "b.md".to_string()),
            ]
        );
    }

    #[test]
    fn grouped_puts_a_chain_of_single_child_folders_on_one_row() {
        let held = entries(&["src/deep/y.md", "src/deep/z.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(
            shape(&rows),
            [
                (0, "src/deep".to_string()),
                (1, "src/deep/y.md".to_string()),
                (1, "src/deep/z.md".to_string()),
            ]
        );

        // An untracked nested repository is one entry ending in `/`, and it is a row of its own
        // rather than a folder with nothing inside it.
        let held = entries(&["newdir/"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(shape(&rows), [(0, "newdir/".to_string())]);
    }

    #[test]
    fn a_collapsed_folder_drops_everything_under_it_and_only_in_its_own_section() {
        let held = entries(&["src/x.md", "a.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let collapsed = HashSet::from([folder_key(Section::Changes, "src")]);
        let rows = grouped(&refs, Section::Changes, &collapsed, &identity);
        assert_eq!(
            shape(&rows),
            [(0, "src".to_string()), (0, "a.md".to_string())]
        );
        // The row says so itself, so a refresh that keeps it cannot keep a stale chevron.
        assert!(matches!(rows[0], Row::Folder { open: false, .. }));
        assert_eq!(
            shape(&grouped(&refs, Section::Staged, &collapsed, &identity)).len(),
            3,
            "the same folder under another section is its own row"
        );
    }

    #[test]
    fn a_folder_offers_discard_only_where_every_file_under_it_can_go() {
        let held = [entry("src/a.md", '.', 'M'), entry("src/new.md", '?', '?')];
        let refs: Vec<&Entry> = held.iter().collect();
        let outside = |path: &str| format!("/elsewhere/{path}");
        let offered = |key: &dyn Fn(&str) -> String| {
            let rows = grouped(&refs, Section::Changes, &HashSet::new(), key);
            matches!(rows[0], Row::Folder { discardable, .. } if discardable)
        };
        assert!(offered(&identity), "trashed, being in the vault");
        assert!(!offered(&outside), "nowhere to go outside it");

        // A repository of its own is skipped by its folder's Discard, and offers none itself.
        let held = [entry("src/a.md", '.', 'M'), entry("src/sub/", '?', '?')];
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert!(matches!(rows[0], Row::Folder { discardable, .. } if discardable));
        assert!(!discardable(&held[1], "src/sub/"));
        assert!(!discards(&refs[1..], &identity), "nothing left to take");
    }

    #[test]
    fn the_flat_view_is_the_list_git_gave_us() {
        let status = Status {
            entries: vec![entry("src/x.md", '.', 'M'), entry("a.md", '.', 'M')],
            ..Status::default()
        };
        let rows = rows_of(&status, &[], &identity, false, &HashSet::new());
        assert_eq!(
            shape(&rows[1..]),
            [(0, "src/x.md".to_string()), (0, "a.md".to_string())]
        );
    }

    #[test]
    fn a_file_deleted_from_the_working_tree_is_compared_against_nothing() {
        let sides = |section, x, y| sides_for(section, &entry("f.md", x, y));
        assert!(matches!(
            sides(Section::Changes, '.', 'D'),
            Some(Sides::Deleted)
        ));
        assert!(matches!(
            sides(Section::Changes, '.', 'M'),
            Some(Sides::Worktree)
        ));
        assert!(matches!(
            sides(Section::Staged, 'D', '.'),
            Some(Sides::Staged { orig: None })
        ));
        assert!(sides(Section::Conflicts, 'U', 'U').is_none());
    }

    #[test]
    fn a_staged_rename_reads_head_at_the_path_it_came_from() {
        let mut renamed = entry("new.md", 'R', 'M');
        renamed.orig = Some("old.md".to_string());
        assert!(matches!(
            sides_for(Section::Staged, &renamed),
            Some(Sides::Staged { orig: Some(orig) }) if orig == "old.md"
        ));
    }

    #[test]
    fn a_file_at_the_root_of_the_folder_view_is_inset_like_every_other_file() {
        assert_eq!(inset(0, true), FILE_INSET, "under a root folder's icon");
        assert_eq!(inset(2, true), 2 * INDENT + FILE_INSET);
        assert_eq!(inset(0, false), 0, "the flat view is not indented");
    }

    #[test]
    fn status_letter_reads_the_side_its_section_is_about() {
        let renamed = entry("a.md", 'R', 'M');
        assert_eq!(status_letter(&renamed, Section::Staged), 'R');
        assert_eq!(status_letter(&renamed, Section::Changes), 'M');
        assert_eq!(
            status_letter(&entry("n.md", '?', '?'), Section::Changes),
            'U'
        );
    }
}
