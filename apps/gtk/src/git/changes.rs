//! The changed-files list: the rows one `git status` becomes, and the widgets they are drawn in.

use super::compare::Sides;
use super::*;

/// How far one level of the changes tree is indented, in px. `GtkTreeExpander`'s own step, so a
/// folder here sits where the same folder sits in the Files tree.
const INDENT: i32 = 16;

/// How far a file row is inset past the chevron its folder row leads with, in px: the chevron and
/// the box's spacing. Without it a file starts under its folder's icon rather than under its name,
/// and the two read as one column of rows rather than as a tree.
const FILE_INSET: i32 = 22;

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
            if let Some(row) = boxed::<Row>(view.model().and_then(|m| m.item(position))) {
                panel.activate(&row);
            }
        });
    }

    /// Draw the changes list from what the last refresh learned, and nothing else: what the
    /// grouping preference and a folder row both need, neither being a reason to ask git again.
    ///
    /// Only the run of rows that differs is spliced, as the log compares before it draws: a save,
    /// a watcher event and the `.git` write a Stage makes each land a refresh that mostly says
    /// what is on screen already, and a row spliced out from under a press loses its release —
    /// which is how Stage clicks went missing. So a row has to carry everything its binding
    /// draws; the one thing it does not, the view, empties the list in [`Panel::set_tree`].
    /// Nothing moves while a press is down over the list: its release redraws.
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
                    &|path| vault_key(&self.hooks.vault.root(), repo, path),
                    self.tree.get(),
                    &self.collapsed.borrow(),
                ),
                _ => Vec::new(),
            }
        };
        let held: Vec<Row> = (0..self.changes.n_items())
            .filter_map(|i| boxed(self.changes.item(i)))
            .collect();
        let (at, removed, added) = changed_run(&held, &rows);
        if removed == 0 && added == 0 {
            return;
        }
        let items: Vec<glib::BoxedAnyObject> = rows
            .into_iter()
            .skip(at)
            .take(added)
            .map(glib::BoxedAnyObject::new)
            .collect();
        self.changes.splice(at as u32, removed as u32, &items);
    }

    /// Activate the row `path` is listed on, as a click on it does, and say which section it was
    /// in. `ACCENT_BENCH_COMPARE=row:` and nothing else: the headless image has no pointer.
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
            None => (self.hooks.open)(key),
        }
    }
}

/// What activating a changed file compares, or `None` for a conflict, which is resolved in the
/// file rather than in a diff of two sides that both lost. A file deleted from the working tree
/// has no tab to compare inside, so it gets one of its own: the index against nothing.
fn sides_for(section: Section, entry: &Entry) -> Option<Sides> {
    match section {
        Section::Conflicts => None,
        Section::Staged => Some(Sides::Staged),
        Section::Changes if entry.y == 'D' => Some(Sides::Deleted),
        Section::Changes => Some(Sides::Worktree),
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
            let paths = panel.section_paths(section, "");
            match section {
                Section::Staged => panel.unstage(paths),
                _ => panel.stage(paths),
            }
        }
    ));
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&all);

    // A folder of the tree view: the chevron says whether it is open, the folder icon says it is
    // one — the same icon the Files tree gives a directory — and the label carries whatever
    // segments this row adds to the one above it.
    let chevron = gtk::Image::new();
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

    let entry = file_line();
    entry.append(&actions(item, panel));

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

/// The marker class on a list row whose buttons already follow its hover ([`reveal_on_hover`]).
const WATCHED: &str = "git-row";

/// Which of a row's three buttons was pressed.
#[derive(Clone, Copy)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

/// Show a row's buttons while the pointer or the keyboard is on it, and give them no width at all
/// the rest of the time, so the name beside them reads out to the whole width of the pane and is
/// cut short only where there is really something to give way to. `.git-actions` fades them in
/// and out; what it cannot do is stop them reserving their room, a `GtkRevealer` that is shut
/// measuring nothing.
///
/// Watched on the list row rather than on the stack inside it: that is the widget GTK marks with
/// PRELIGHT while the pointer is anywhere on it and with FOCUS_WITHIN while one of its buttons has
/// the keyboard — and it is the one the keyboard lands on first, so Tab reveals the buttons it
/// would otherwise never be able to reach.
fn reveal_on_hover(row: &gtk::Widget) {
    // Once per list row widget, which is recycled and bound again and again. The class is the
    // marker, there being nowhere else to keep one bit on a widget GTK made for itself.
    // ponytail: it is also a hook if a row of this list ever wants styling of its own.
    if row.has_css_class(WATCHED) {
        return;
    }
    row.add_css_class(WATCHED);
    row.connect_state_flags_changed(|row, _| {
        let on = row.state_flags().intersects(
            gtk::StateFlags::PRELIGHT | gtk::StateFlags::FOCUS_WITHIN | gtk::StateFlags::FOCUSED,
        );
        for revealer in revealers(row) {
            revealer.set_reveal_child(on);
        }
    });
}

/// Every [`actions`] revealer under `row` — one per layout the row's stack can show.
fn revealers(row: &gtk::Widget) -> Vec<gtk::Revealer> {
    let mut found = Vec::new();
    let mut todo = vec![row.clone()];
    while let Some(widget) = todo.pop() {
        let widget = match widget.downcast::<gtk::Revealer>() {
            Ok(revealer) => {
                found.push(revealer);
                continue;
            }
            Err(widget) => widget,
        };
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    found
}

/// A row's Stage / Unstage / Discard buttons, the same three on a file and on a folder. They show
/// on the row's hover and `:focus-within` (`.git-actions`), and the binder picks which of them the
/// row offers. In a revealer, which is what keeps them from reserving their width while they are
/// away ([`reveal_on_hover`]).
fn actions(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Revealer {
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
                if let (Some(panel), Some(row)) = (weak.upgrade(), boxed::<Row>(item.item())) {
                    panel.act(act, row);
                }
            }
        ));
        actions.append(&button);
    }
    gtk::Revealer::builder()
        .child(&actions)
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .build()
}

/// The buttons inside an [`actions`] revealer, which is what the binder sets up.
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

fn bind_change(item: &gtk::ListItem, panel: &Weak<Panel>) {
    let (Some(stack), Some(row), Some(panel)) = (
        item.child().and_downcast::<gtk::Stack>(),
        boxed::<Row>(item.item()),
        panel.upgrade(),
    ) else {
        return;
    };
    // The list row itself, which is not there at setup — `set_child` only stores the widget and
    // the row it goes in is made when the item is first bound — and must not be touched from
    // inside the bind either: a class added or a handler connected on it there left the list
    // manager handing GTK null children, a `gtk_widget_insert_after: assertion 'GTK_IS_WIDGET'`
    // per row. An idle is past the manager's pass and costs one closure per bind.
    let list_row = stack.parent();
    glib::idle_add_local_once(move || {
        if let Some(list_row) = list_row {
            reveal_on_hover(&list_row);
        }
    });
    let (Some(header), Some(folder), Some(entry)) = (
        stack.child_by_name("header").and_downcast::<gtk::Box>(),
        stack.child_by_name("folder").and_downcast::<gtk::Box>(),
        stack.child_by_name("entry").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let Some(title) = header.first_child().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(all) = title.next_sibling().and_downcast::<gtk::Button>() else {
        return;
    };
    let Some(actions) = buttons(entry.last_child()) else {
        return;
    };

    match row {
        Row::Header {
            title: text,
            all: section,
        } => {
            stack.set_visible_child_name("header");
            stack.set_tooltip_text(None);
            title.set_text(text);
            all.set_visible(section.is_some());
            all.set_label(match section {
                Some(Section::Staged) => "Unstage All",
                _ => "Stage All",
            });
        }
        Row::Folder {
            label,
            section,
            depth,
            path,
            open,
            discardable,
        } => {
            stack.set_visible_child_name("folder");
            let (Some(chevron), Some(buttons)) = (
                folder.first_child().and_downcast::<gtk::Image>(),
                buttons(folder.last_child()),
            ) else {
                return;
            };
            let Some(text) = folder
                .last_child()
                .and_then(|revealer| revealer.prev_sibling())
                .and_downcast::<gtk::Label>()
            else {
                return;
            };
            // None in Merge Conflicts, for the reason its header has no Stage All: a conflict is
            // resolved one file at a time.
            buttons.set_visible(section != Section::Conflicts);
            offer(&buttons, section, discardable);
            // The fold chevrons' pair rather than `pan-*`, for the reason the branch button gives.
            chevron.set_icon_name(Some(match open {
                true => "go-down-symbolic",
                false => "go-next-symbolic",
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
            entry.set_margin_start(inset(depth, panel.tree.get()));
            let directory = match depth {
                0 => split_name(&e.path).0,
                _ => "",
            };
            let icon = crate::doc::icon_for(&e.path);
            bind_file_line(&entry, icon, status_letter(&e, section), &e.path, directory);
            stack.set_tooltip_text(Some(&e.path));
            actions.set_visible(true);
            offer(&actions, section, discardable(&e, &key));
        }
        Row::Submodule(sub) => {
            stack.set_visible_child_name("entry");
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
            actions.set_visible(false);
        }
    }
}

/// Which of the [`actions`] a file or folder row offers in `section`. Staging a conflicted file
/// is how git is told it is resolved, so Stage is there for it too; unstaging one is not a thing
/// the pane offers. Discard is for the working tree alone, and only where everything the row
/// stands for can go ([`discardable`]).
fn offer(actions: &gtk::Box, section: Section, discardable: bool) {
    let Some(stage) = actions.first_child() else {
        return;
    };
    let (unstage, discard) = (stage.next_sibling(), actions.last_child());
    stage.set_visible(section != Section::Staged);
    if let (Some(unstage), Some(discard)) = (unstage, discard) {
        unstage.set_visible(section == Section::Staged);
        discard.set_visible(section == Section::Changes && discardable);
    }
}

/// Whether Discard can take `entry` back, `key` being its path as the app names it.
///
/// ponytail: an untracked file outside the vault cannot, because the only thing to do with it is
/// delete it and the trash hook takes vault keys; a folder with one under it offers no Discard
/// either. `git clean` is the upgrade, and it wants a confirmation naming the files it removes
/// for good.
fn discardable(entry: &Entry, key: &str) -> bool {
    entry.x != '?' || !Path::new(key).is_absolute()
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
        let open = !collapsed.contains(&folder_key(section, &path));
        rows.push(Row::Folder {
            label,
            section,
            depth,
            path: path.clone(),
            open,
            discardable: group.iter().all(|e| discardable(e, &key(&e.path))),
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

/// The one run of `rows` that differs from `held`: where it starts, how many of `held` it replaces
/// and how many of `rows` replace them. What the rows before and after it share is left out, so
/// their widgets stay where they are.
fn changed_run<T: PartialEq>(held: &[T], rows: &[T]) -> (usize, usize, usize) {
    let head = held.iter().zip(rows).take_while(|(a, b)| a == b).count();
    let tail = held[head..]
        .iter()
        .rev()
        .zip(rows[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (head, held.len() - head - tail, rows.len() - head - tail)
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
    fn changed_run_leaves_the_rows_either_side_of_a_change_alone() {
        assert_eq!(changed_run(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0), "nothing");
        // Staging `b` opens a Staged section above and takes `b` out of Changes: the rows after
        // it keep their widgets.
        assert_eq!(
            changed_run(&["C", "a", "b", "c", "d"], &["S", "b", "C", "a", "c", "d"]),
            (0, 3, 4)
        );
        assert_eq!(changed_run(&[1, 2, 3], &[1, 3]), (1, 1, 0), "a row gone");
        assert_eq!(changed_run(&[1, 3], &[1, 2, 3]), (1, 0, 1), "a row come");
        assert_eq!(changed_run(&[1, 1], &[1, 1, 1]), (2, 0, 1), "no overlap");
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
            Some(Sides::Staged)
        ));
        assert!(sides(Section::Conflicts, 'U', 'U').is_none());
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
