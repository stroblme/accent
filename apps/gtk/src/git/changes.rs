//! The changed-files list: the rows one `git status` becomes, and the widgets they are drawn in.

use super::compare::Sides;
use super::*;

/// How far one level of the changes tree is indented, in px.
const INDENT: i32 = 12;

/// Which list a row belongs to, which is what decides the letter it shows, the buttons it offers
/// and what activating it compares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Section {
    Conflicts,
    Staged,
    Changes,
}

/// One line of the changes list. Headers are rows of their own rather than list sections, so the
/// whole thing is one flat `ListStore` and an empty section is simply two rows that are not there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Header {
        title: &'static str,
        /// The section a "Stage All" / "Unstage All" button acts on, or `None` for a header with
        /// no bulk action: conflicts are resolved one file at a time, and submodules are a
        /// read-only list.
        all: Option<Section>,
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
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&change_row(item, &weak)));
            }
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_change(item, &weak);
            }
        });
        view.set_factory(Some(&factory));

        let weak = Rc::downgrade(self);
        view.connect_activate(move |view, position| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if let Some(row) = boxed::<Row>(view.model().and_then(|m| m.item(position))) {
                panel.activate(&row);
            }
        });
    }

    /// Draw the changes list from what the last refresh learned, and nothing else: what the tree
    /// toggle and a folder row both need, neither of them being a reason to ask git again.
    pub(super) fn rebuild_changes(&self) {
        let rows = {
            let state = self.state.borrow();
            match (
                state.statuses.get(state.selected),
                state.repos.get(state.selected),
            ) {
                (Some(status), Some(repo)) => rows_of(
                    status,
                    &state.submodules,
                    &|path| vault_key(&self.hooks.vault.root(), repo, path),
                    self.tree.get(),
                    &self.collapsed.borrow(),
                ),
                _ => Vec::new(),
            }
        };
        let items: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.changes.splice(0, self.changes.n_items(), &items);
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
        match section {
            // A conflict is resolved in the file, not in a diff of two sides that both lost.
            Section::Conflicts => (self.hooks.open)(key),
            Section::Staged => self.compare(&entry.path, key, Sides::Staged),
            Section::Changes => self.compare(&entry.path, key, Sides::Worktree),
        }
    }
}

/// One changes row: a header layout and an entry layout in a stack, so a recycled row can be
/// either. The buttons hold the `GtkListItem` rather than the row's data, because the data is
/// replaced under them every time the row is reused.
fn change_row(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Stack {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    let all = gtk::Button::builder().build();
    all.add_css_class("flat");
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
            let paths = panel.section_paths(section);
            match section {
                Section::Staged => panel.unstage(paths),
                _ => panel.stage(paths),
            }
        }
    ));
    // One preference with two surfaces: this and the switch in Preferences write the same
    // `git_tree`. It rides the Changes header because that is where the list it reshapes begins,
    // and the binder puts it back on the preference every time the row is reused — which is why
    // the handler below has to recognise its own echo and do nothing.
    let view = gtk::ToggleButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Group changed files by folder")
        .valign(gtk::Align::Center)
        .build();
    view.add_css_class("flat");
    let weak = panel.clone();
    view.connect_toggled(move |button| {
        let Some(panel) = weak.upgrade() else {
            return;
        };
        let on = button.is_active();
        if panel.tree.replace(on) == on {
            return;
        }
        (panel.hooks.set_tree)(on);
        panel.rebuild_changes();
    });

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&all);
    header.append(&view);

    // A folder of the tree view: the chevron says whether it is open, the label carries whatever
    // segments this row adds to the one above it.
    let chevron = gtk::Image::new();
    let folder_name = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    let folder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    folder.append(&chevron);
    folder.append(&folder_name);

    let entry = file_line();

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.add_css_class("git-actions");
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
                let (Some(panel), Some(Row::Entry { entry, key, .. })) =
                    (weak.upgrade(), boxed(item.item()))
                else {
                    return;
                };
                match act {
                    Act::Stage => panel.stage(vec![entry.path]),
                    Act::Unstage => panel.unstage(vec![entry.path]),
                    Act::Discard => panel.discard(&entry, &key),
                }
            }
        ));
        actions.append(&button);
    }

    entry.append(&actions);

    // Not homogeneous: the header's button is taller than an entry row, and every row taking that
    // height would turn the list into a ladder.
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&header, Some("header"));
    stack.add_named(&folder, Some("folder"));
    stack.add_named(&entry, Some("entry"));
    stack
}

/// Which of a row's three buttons was pressed.
#[derive(Clone, Copy)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

fn bind_change(item: &gtk::ListItem, panel: &Weak<Panel>) {
    let (Some(stack), Some(row), Some(panel)) = (
        item.child().and_downcast::<gtk::Stack>(),
        boxed::<Row>(item.item()),
        panel.upgrade(),
    ) else {
        return;
    };
    let (Some(header), Some(folder), Some(entry)) = (
        stack.child_by_name("header").and_downcast::<gtk::Box>(),
        stack.child_by_name("folder").and_downcast::<gtk::Box>(),
        stack.child_by_name("entry").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(title), Some(view)) = (
        header.first_child().and_downcast::<gtk::Label>(),
        header.last_child().and_downcast::<gtk::ToggleButton>(),
    ) else {
        return;
    };
    let Some(all) = title.next_sibling().and_downcast::<gtk::Button>() else {
        return;
    };
    let Some(actions) = entry.last_child().and_downcast::<gtk::Box>() else {
        return;
    };

    match row {
        Row::Header {
            title: text,
            all: section,
        } => {
            stack.set_visible_child_name("header");
            title.set_text(text);
            all.set_visible(section.is_some());
            all.set_label(match section {
                Some(Section::Staged) => "Unstage All",
                _ => "Stage All",
            });
            // One toggle for the whole list, on the section it is most about. Setting it here is
            // what the handler in `change_row` reads back as its own echo.
            view.set_visible(section == Some(Section::Changes));
            view.set_active(panel.tree.get());
        }
        Row::Folder {
            label,
            section,
            depth,
            path,
        } => {
            stack.set_visible_child_name("folder");
            let (Some(chevron), Some(text)) = (
                folder.first_child().and_downcast::<gtk::Image>(),
                folder.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let shut = panel
                .collapsed
                .borrow()
                .contains(&folder_key(section, &path));
            chevron.set_icon_name(Some(match shut {
                true => "pan-end-symbolic",
                false => "pan-down-symbolic",
            }));
            text.set_text(&label);
            folder.set_margin_start(depth as i32 * INDENT);
            stack.set_tooltip_text(Some(&path));
        }
        Row::Entry {
            entry: e,
            section,
            key,
            depth,
        } => {
            stack.set_visible_child_name("entry");
            entry.set_margin_start(depth as i32 * INDENT);
            let directory = match depth {
                0 => split_name(&e.path).0,
                _ => "",
            };
            bind_file_line(&entry, status_letter(&e, section), &e.path, directory);
            stack.set_tooltip_text(Some(&e.path));
            actions.set_visible(true);
            let Some((stage, unstage, discard)) = triple(&actions) else {
                return;
            };
            // Staging a conflicted file is how git is told it is resolved, so the button is there
            // for it too; unstaging one is not a thing the pane offers.
            stage.set_visible(section != Section::Staged);
            unstage.set_visible(section == Section::Staged);
            // ponytail: an untracked file outside the vault has no Discard, because the only
            // thing to do with it is delete it and the trash hook takes vault keys. `git clean`
            // is the upgrade, and it wants a confirmation naming the file it removes for good.
            discard.set_visible(
                section == Section::Changes && (e.x != '?' || !Path::new(&key).is_absolute()),
            );
        }
        Row::Submodule(sub) => {
            stack.set_visible_child_name("entry");
            entry.set_margin_start(0);
            let directory = sub
                .describe
                .as_deref()
                .unwrap_or_else(|| split_name(&sub.path).0);
            bind_file_line(&entry, sub.state, &sub.path, directory);
            stack.set_tooltip_text(Some(&sub.oid));
            actions.set_visible(false);
        }
    }
}

/// The Stage / Unstage / Discard buttons of a row, in the order [`change_row`] appended them.
fn triple(actions: &gtk::Box) -> Option<(gtk::Widget, gtk::Widget, gtk::Widget)> {
    let stage = actions.first_child()?;
    let unstage = stage.next_sibling()?;
    let discard = unstage.next_sibling()?;
    Some((stage, unstage, discard))
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
        rows.push(Row::Header { title, all });
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
        });
        rows.extend(subs.iter().cloned().map(Row::Submodule));
    }
    rows
}

/// What identifies a folder row while it is collapsed. The section is part of it because the same
/// folder can have a row under Staged and another under Changes, and folding one is not folding
/// the other.
fn folder_key(section: Section, dir: &str) -> String {
    format!("{section:?}/{dir}")
}

/// One section's entries grouped by folder.
///
/// Folders come before files at each level and both sets are sorted, which is the order the Files
/// pane's own listing has. A chain of folders with a single child each lands on one row —
/// `src/deep` — as VS Code does it, because a column of rows with one child says nothing. Nothing
/// under a collapsed folder is emitted at all: the list is rebuilt on every toggle.
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
    let mut dirs: Vec<(String, Vec<&Entry>)> = Vec::new();
    let mut files: Vec<&Entry> = Vec::new();
    for &entry in entries {
        match segment(&entry.path, prefix) {
            Some(head) => match dirs.iter_mut().find(|(name, _)| name == head) {
                Some((_, group)) => group.push(entry),
                None => dirs.push((head.to_string(), vec![entry])),
            },
            None => files.push(entry),
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.path.cmp(&b.path));

    for (name, group) in dirs {
        let mut label = name;
        while let Some(only) = only_segment(&group, &format!("{prefix}{label}/")) {
            label = format!("{label}/{only}");
        }
        let path = format!("{prefix}{label}");
        rows.push(Row::Folder {
            label,
            section,
            depth,
            path: path.clone(),
        });
        if !collapsed.contains(&folder_key(section, &path)) {
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
/// folder. git reports a wholly untracked directory as one entry ending in `/`, and that is a row
/// in its own right rather than a folder with nothing inside it.
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
                all: None
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

        // A wholly untracked directory is one entry ending in `/`, and it is a row of its own
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
        assert_eq!(
            shape(&grouped(&refs, Section::Changes, &collapsed, &identity)),
            [(0, "src".to_string()), (0, "a.md".to_string())]
        );
        assert_eq!(
            shape(&grouped(&refs, Section::Staged, &collapsed, &identity)).len(),
            3,
            "the same folder under another section is its own row"
        );
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
