//! The history: the commit rows, the graph beside them, and what a commit expands into.

use super::compare::Sides;
use super::*;

/// The width of one graph lane, in px.
const LANE: i32 = 12;

/// The action group the history's context menu resolves its items against.
const MENU_GROUP: &str = "gitlog";

/// How far a commit the remote has and HEAD does not is faded. Enough to read as "this is not
/// here yet" beside a commit that is, and not so far that the summary stops being legible.
const NOT_PULLED_DIM: f64 = 0.55;

/// One line of the history list. A flat store with two kinds rather than a `GtkTreeListModel`:
/// the log is spliced wholesale on every refresh anyway, so a tree model would only add a
/// create-child-model closure and a placeholder state to keep in step with it.
#[derive(Clone)]
enum LogItem {
    Commit(LogRow),
    /// A file the commit above it changed, shown while that commit is expanded.
    File {
        /// The commit the file belongs to, and its first parent — `None` on a root commit, whose
        /// files have nothing on the left to compare against.
        oid: String,
        parent: Option<String>,
        letter: char,
        path: String,
    },
    /// The last row while git has history the store does not: activating it pages the next
    /// [`PAGE`] in. A row rather than a button under the list, so it is reached by scrolling to
    /// the end of the history it continues.
    More,
}

impl Panel {
    pub(super) fn wire_log(self: &Rc<Self>, view: &gtk::ListView) {
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&log_row(item, &weak)));
            }
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_log(item, &weak);
            }
        });
        view.set_factory(Some(&factory));

        // The same one-click rule as the changes list and the tree: a commit opens its file list,
        // a file in it opens its diff.
        view.set_single_click_activate(true);
        let weak = Rc::downgrade(self);
        view.connect_activate(move |view, position| {
            let (Some(panel), Some(item)) = (
                weak.upgrade(),
                boxed(view.model().and_then(|m| m.item(position))),
            ) else {
                return;
            };
            match item {
                LogItem::Commit(row) => panel.toggle(&row.commit),
                LogItem::File {
                    oid, parent, path, ..
                } => panel.compare(&path, &path, Sides::Commit { oid, parent }),
                LogItem::More => panel.load_more(),
            }
        });
    }

    /// Put `commits` on the graph. `keep` is how many leading rows the store already holds
    /// unchanged: [`git::lanes`] is one forward pass, so a Load More can only append, and
    /// appending leaves the reader where they were instead of scrolling back to the top.
    pub(super) fn fill_log(self: &Rc<Self>, commits: Vec<Commit>, keep: usize) {
        // Which commit was open, so it can be opened again below. A refresh lands on every
        // commit, pull and checkout, and the file list closing under each of them was the one
        // thing about the history that did not survive one.
        let was = self.expanded.borrow().clone();
        self.collapse();
        let rows = git::lanes(commits);
        let keep = keep.min(rows.len()) as u32;
        let mut items: Vec<glib::BoxedAnyObject> = rows[keep as usize..]
            .iter()
            .cloned()
            .map(|row| glib::BoxedAnyObject::new(LogItem::Commit(row)))
            .collect();
        // The splice reaches the end of the store, so this is also what takes the row away again
        // once the last page has come in.
        if self.has_more.get() {
            items.push(glib::BoxedAnyObject::new(LogItem::More));
        }
        self.log
            .splice(keep, self.log.n_items().saturating_sub(keep), &items);
        // After the splice, and only where the commit is still in the page: `toggle` asks git
        // for the file list again and splices it back under the row it now has.
        if let Some(commit) = was.and_then(|oid| rows.iter().find(|r| r.commit.id == oid)) {
            self.toggle(&commit.commit.clone());
        }
    }

    /// Take away whatever file list is open.
    ///
    /// The file rows of one commit are contiguous and only one commit is ever expanded, so where
    /// they are is remembered rather than looked for: a scan would read every row in the store,
    /// and a `LogItem` is several `String`s.
    fn collapse(&self) {
        self.expanded.replace(None);
        if let Some((start, n)) = self.expanded_at.take() {
            self.log.splice(start, n, &[] as &[glib::BoxedAnyObject]);
        }
    }

    /// Show, or hide again, the files one commit changed.
    fn toggle(self: &Rc<Self>, commit: &Commit) {
        let was = self.expanded.borrow().clone();
        self.collapse();
        if was.as_deref() == Some(commit.id.as_str()) {
            return;
        }
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        self.expanded.replace(Some(commit.id.clone()));
        let (oid, parent) = (commit.id.clone(), commit.parents.first().cloned());
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let query = oid.clone();
            let vault = panel.hooks.vault.clone();
            let files = gio::spawn_blocking(move || vault.git_changed_files(&repo, &query)).await;
            let files = match files {
                Ok(Ok(files)) => files,
                Ok(Err(e)) => return tracing::debug!("git show --name-status: {e}"),
                Err(_) => return tracing::warn!("the git worker panicked"),
            };
            // A refresh, or another commit, may have landed while git was answering.
            if panel.expanded.borrow().as_deref() != Some(oid.as_str()) {
                return;
            }
            let Some(at) = panel.row_of_commit(&oid) else {
                return;
            };
            let rows: Vec<glib::BoxedAnyObject> = files
                .into_iter()
                .map(|(letter, path)| {
                    glib::BoxedAnyObject::new(LogItem::File {
                        oid: oid.clone(),
                        parent: parent.clone(),
                        letter,
                        path,
                    })
                })
                .collect();
            panel.expanded_at.set(Some((at + 1, rows.len() as u32)));
            panel.log.splice(at + 1, 0, &rows);
        });
    }

    /// Where a commit sits in the log store, or `None` if it has since been spliced away.
    fn row_of_commit(&self, oid: &str) -> Option<u32> {
        (0..self.log.n_items()).find(|i| {
            peek(
                self.log.item(*i),
                |item: &LogItem| matches!(item, LogItem::Commit(row) if row.commit.id == oid),
            ) == Some(true)
        })
    }

    fn load_more(self: &Rc<Self>) {
        let (repo, skip) = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => (repo.clone(), state.commits.len()),
                None => return,
            }
        };
        // Cleared for the whole hop: the row stays where it is, and activating it again while
        // the page is in flight finds nothing left to ask for.
        if !self.has_more.replace(false) {
            return;
        }
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let vault = panel.hooks.vault.clone();
            let page = gio::spawn_blocking(move || vault.git_log(&repo, skip, PAGE)).await;
            let page = match page {
                Ok(Ok(page)) => page,
                // Whatever went wrong, the history behind the row is still there, so it stays.
                Ok(Err(e)) => {
                    tracing::debug!("git log: {e}");
                    return panel.has_more.set(true);
                }
                Err(_) => {
                    tracing::warn!("the git worker panicked");
                    return panel.has_more.set(true);
                }
            };
            panel.has_more.set(page.len() >= PAGE);
            let commits = {
                let mut state = panel.state.borrow_mut();
                state.commits.extend(page);
                state.commits.clone()
            };
            // `skip` is how many commit rows the store already had, which after the collapse
            // inside `fill_log` is exactly how many of them stay.
            panel.fill_log(commits, skip);
        });
    }

    /// What can be done with the commit under the pointer.
    ///
    /// The shape `fileops::context_menu` uses, and for the reasons documented there: the popover
    /// hangs off a layout-managed box rather than off the list, the actions live on that same box
    /// so an item can resolve them, and the unparent waits for an idle because `closed` is emitted
    /// from inside the item's own click and an unparented popover has no path to the action group.
    fn commit_menu(self: &Rc<Self>, oid: &str, anchor: gdk::Rectangle) {
        self.column
            .insert_action_group(MENU_GROUP, Some(&self.commit_actions()));

        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Check Out Commit", "checkout-commit", oid));
        // Its own section: reading an id out is not a thing that moves HEAD.
        let copy = gio::Menu::new();
        copy.append_item(&menu_item("Copy Commit ID", "copy-id", oid));
        menu.append_section(None, &copy);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        // The sidebar behind it is a list, so the menu needs a background of its own.
        popover.add_css_class("git-menu");
        popover.set_parent(&self.column);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&anchor));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// The two actions the menu items name, each taking the commit's id as its parameter.
    fn commit_actions(self: &Rc<Self>) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        for (name, detach) in [("checkout-commit", true), ("copy-id", false)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, target| {
                let (Some(panel), Some(oid)) = (weak.upgrade(), target.and_then(|t| t.str()))
                else {
                    return;
                };
                match detach {
                    true => panel.detach(oid.to_string()),
                    // No toast for the clipboard alone would be truer to DESIGN.md, but nothing
                    // else on screen says the id was taken: the row looks the same either way.
                    false => {
                        panel.hooks.window.clipboard().set_text(oid);
                        (panel.hooks.toast)(&format!("Copied {}", short(oid)));
                    }
                }
            });
            group.add_action(&action);
        }
        group
    }
    /// How many history rows are drawn as not pulled yet. `ACCENT_BENCH_GIT` and nothing else:
    /// the marking is otherwise only visible as a faded row.
    pub fn not_pulled_rows(&self) -> usize {
        let incoming = &self.state.borrow().incoming;
        (0..self.log.n_items())
            .filter(|i| {
                peek(self.log.item(*i), |item: &LogItem| {
                    matches!(item, LogItem::Commit(row) if incoming.contains(&row.commit.id))
                }) == Some(true)
            })
            .count()
    }
}

/// One log row: the graph on the left, the summary and its author on the right.
fn log_row(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Stack {
    let area = gtk::DrawingArea::new();
    // The draw reads the bound row straight off the list item, so a recycled row cannot draw the
    // graph of the commit that used to be in it.
    area.set_draw_func(glib::clone!(
        #[weak]
        item,
        move |_, cr, _, height| {
            if let Some(LogItem::Commit(row)) = boxed(item.item()) {
                draw_lanes(cr, &row, height as f64);
            }
        }
    ));

    // Ellipsized like every other name in the pane: a decoration is as long as the branch it
    // names, and without this a long branch is the sidebar's floor.
    let refs = gtk::Label::builder()
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for class in ["caption", "dim-label"] {
        refs.add_css_class(class);
    }
    let summary = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    // The same arrow the branch readout's `↓2` uses, so one symbol means "the remote has this and
    // we do not" in both places. Leading, where a dirty tab and the status bar put their dot.
    let not_pulled = gtk::Label::new(Some("↓"));
    for class in ["caption", "dim-label", "numeric"] {
        not_pulled.add_css_class(class);
    }
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.append(&not_pulled);
    line.append(&refs);
    line.append(&summary);

    let meta = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for class in ["caption", "dim-label"] {
        meta.add_css_class(class);
    }

    // The row's own vertical margin is off, so the breathing room lives here (see the `.git-log`
    // rule): the drawing area has to reach the row's edges for the lanes to join.
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    text.append(&line);
    text.append(&meta);

    let commit = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    commit.append(&area);
    commit.append(&text);

    // A file of the expanded commit, indented past the graph so it reads as belonging above it.
    let file = file_line();
    file.set_margin_start(LANE * 2);
    file.set_margin_top(2);
    file.set_margin_bottom(2);

    // Centred and quiet: it continues the history above it rather than competing with it.
    let more = gtk::Label::builder()
        .label("Load More")
        .margin_top(6)
        .margin_bottom(6)
        .build();
    for class in ["caption", "dim-label"] {
        more.add_css_class(class);
    }

    // Not homogeneous, for the reason `change_row` gives: a commit row is two lines tall and a
    // file row one, and every row taking the taller of the two would be a ladder.
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&commit, Some("commit"));
    stack.add_named(&file, Some("file"));
    stack.add_named(&more, Some("more"));

    // A secondary click on a commit opens its menu. The gesture holds the `GtkListItem` rather
    // than the row's data, for the reason `change_row`'s buttons do: the data under a recycled
    // row is replaced without the widgets being rebuilt.
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let weak = panel.clone();
    click.connect_pressed(glib::clone!(
        #[weak]
        item,
        move |gesture, _, x, y| {
            let (Some(panel), Some(LogItem::Commit(row))) = (weak.upgrade(), boxed(item.item()))
            else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            // Out of the row's coordinates and into the host box's, or the menu would point at
            // wherever that row happened to be when the list was last scrolled.
            let point = gtk::graphene::Point::new(x as f32, y as f32);
            let Some(at) = item
                .child()
                .and_then(|child| child.compute_point(&panel.column, &point))
            else {
                return;
            };
            let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
            panel.commit_menu(&row.commit.id, anchor);
        }
    ));
    stack.add_controller(click);
    stack
}

fn bind_log(item: &gtk::ListItem, panel: &Weak<Panel>) {
    let (Some(stack), Some(item_row)) = (
        item.child().and_downcast::<gtk::Stack>(),
        boxed::<LogItem>(item.item()),
    ) else {
        return;
    };
    let (Some(commit), Some(file)) = (
        stack.child_by_name("commit").and_downcast::<gtk::Box>(),
        stack.child_by_name("file").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };

    let row = match item_row {
        LogItem::Commit(row) => row,
        LogItem::File { letter, path, .. } => {
            stack.set_visible_child_name("file");
            let icon = crate::doc::icon_for(&path);
            bind_file_line(&file, icon, letter, &path, split_name(&path).0);
            stack.set_tooltip_text(Some(&path));
            return;
        }
        LogItem::More => {
            stack.set_visible_child_name("more");
            stack.set_tooltip_text(None);
            return;
        }
    };

    stack.set_visible_child_name("commit");
    let (Some(area), Some(text)) = (
        commit.first_child().and_downcast::<gtk::DrawingArea>(),
        commit.last_child().and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(line), Some(meta)) = (
        text.first_child().and_downcast::<gtk::Box>(),
        text.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let (Some(not_pulled), Some(summary)) = (
        line.first_child().and_downcast::<gtk::Label>(),
        line.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let Some(refs) = not_pulled.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };

    area.set_content_width(lane_width(&row));
    area.queue_draw();
    refs.set_visible(!row.commit.refs.is_empty());
    refs.set_text(&row.commit.refs.join(", "));
    summary.set_text(&row.commit.summary);
    meta.set_text(&format!(
        "{} · {}",
        row.commit.author,
        ago(now(), row.commit.time)
    ));
    // Read off the last refresh's answer rather than stored on the row: `git rev-list HEAD..@{u}`
    // is what decides this, and a row that has since been pulled is marked by the refresh that
    // noticed, not by whatever was true when it was spliced in.
    let waiting = panel
        .upgrade()
        .is_some_and(|panel| panel.state.borrow().incoming.contains(&row.commit.id));
    not_pulled.set_visible(waiting);
    // The text alone, so the graph the drawing area beside it paints stays at full strength and a
    // lane still joins the rows above and below.
    text.set_opacity(match waiting {
        true => NOT_PULLED_DIM,
        false => 1.0,
    });
    stack.set_tooltip_text(Some(&match waiting {
        true => format!("Not pulled yet\n\n{}", commit_tooltip(&row.commit)),
        false => commit_tooltip(&row.commit),
    }));
}

/// The graph: the lanes passing this row, the edges into and out of this commit, and the node.
///
/// `above` and `below` are lane indices and `below[0]` is the commit's own column, so an edge is
/// always drawn between a lane's x and the node's, meeting at the row's middle.
fn draw_lanes(cr: &gtk::cairo::Context, row: &LogRow, height: f64) {
    let x = |lane: usize| (LANE / 2 + lane as i32 * LANE) as f64;
    let (middle, node) = (height / 2.0, x(row.column));
    cr.set_line_width(1.5);
    for &lane in &row.through {
        lane_source(cr, lane);
        cr.move_to(x(lane), 0.0);
        cr.line_to(x(lane), height);
        let _ = cr.stroke();
    }
    for &lane in &row.above {
        lane_source(cr, lane);
        cr.move_to(x(lane), 0.0);
        cr.line_to(node, middle);
        let _ = cr.stroke();
    }
    for &lane in &row.below {
        lane_source(cr, lane);
        cr.move_to(node, middle);
        cr.line_to(x(lane), height);
        let _ = cr.stroke();
    }
    lane_source(cr, row.column);
    cr.arc(node, middle, 3.5, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}

fn lane_source(cr: &gtk::cairo::Context, lane: usize) {
    let colour = highlight::lane_colour(lane);
    cr.set_source_rgba(
        colour.red() as f64,
        colour.green() as f64,
        colour.blue() as f64,
        colour.alpha() as f64,
    );
}

/// Room for every lane this row touches, plus one lane of air before the text.
fn lane_width(row: &LogRow) -> i32 {
    let widest = row
        .above
        .iter()
        .chain(&row.below)
        .chain(&row.through)
        .chain(std::iter::once(&row.column))
        .copied()
        .max()
        .unwrap_or(0);
    (widest as i32 + 1) * LANE + LANE
}

/// One context-menu item carrying its commit id as a `String` target rather than in a
/// detailed-action string, which is the shape `fileops::item` settled on.
fn menu_item(label: &str, action: &str, oid: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(
        Some(&format!("{MENU_GROUP}.{action}")),
        Some(&oid.to_variant()),
    );
    item
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// How long ago, in one field. Never more precise than the reader can use: a commit from this
/// morning is "5 h", not "5 h 12 min".
fn ago(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    let (minute, hour, day) = (60, 3600, 86_400);
    // 30 and 365 days, which is what every relative timestamp means by a month and a year.
    let (month, year) = (30 * day, 365 * day);
    if seconds < minute {
        "just now".to_string()
    } else if seconds < hour {
        format!("{} min", seconds / minute)
    } else if seconds < day {
        format!("{} h", seconds / hour)
    } else if seconds < month {
        format!("{} d", seconds / day)
    } else if seconds < year {
        format!("{} mo", seconds / month)
    } else {
        format!("{} y", seconds / year)
    }
}

/// What hovering a commit says: where it sits, what it is called, and the whole message.
///
/// The decorations `git log` already fetched rather than a `git branch --contains` per hover, so
/// a commit that is no branch tip simply has no first line.
fn commit_tooltip(c: &Commit) -> String {
    let head = match c.refs.is_empty() {
        true => short(&c.id),
        false => format!("{}\n{}", c.refs.join(", "), short(&c.id)),
    };
    let message = match c.body.is_empty() {
        true => c.summary.clone(),
        false => format!("{}\n\n{}", c.summary, c.body),
    };
    format!("{head}\n\n{message}")
}

/// Whether a freshly-read first page says the history has not moved: the same commits, whole and
/// in the same order, at the head of what the pane already holds. Whole commits and not their ids
/// alone, so that a branch moving onto a commit — a decoration, and nothing else — still redraws.
///
/// A page that is longer than what is held is a first refresh or a shorter history; either way it
/// has to be drawn. A page that is shorter is what a Load More leaves behind, and its own rows
/// stay where they are.
pub(super) fn same_head(held: &[Commit], page: &[Commit]) -> bool {
    held.len() >= page.len() && held[..page.len()] == *page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_says_one_thing_per_scale() {
        let day = 86_400;
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(ago(1059, 1000), "just now");
        assert_eq!(ago(1060, 1000), "1 min");
        assert_eq!(ago(1000 + 5 * 60, 1000), "5 min");
        assert_eq!(ago(1000 + 3 * 3600, 1000), "3 h");
        assert_eq!(ago(1000 + 2 * day, 1000), "2 d");
        assert_eq!(ago(1000 + 120 * day, 1000), "4 mo");
        assert_eq!(ago(1000 + 400 * day, 1000), "1 y");
        assert_eq!(ago(1000, 2000), "just now", "a clock skew is not a future");
    }

    fn commit_at(id: &str) -> Commit {
        Commit {
            id: id.to_string(),
            parents: Vec::new(),
            refs: Vec::new(),
            author: "a".to_string(),
            time: 0,
            summary: "s".to_string(),
            body: String::new(),
        }
    }

    #[test]
    fn commit_tooltip_says_where_the_commit_is_and_what_it_says() {
        let mut c = commit_at("abcdef1234567");
        c.summary = "subject".to_string();
        assert_eq!(commit_tooltip(&c), "abcdef1\n\nsubject");

        c.body = "why it happened\nand a second line".to_string();
        assert_eq!(
            commit_tooltip(&c),
            "abcdef1\n\nsubject\n\nwhy it happened\nand a second line"
        );

        c.refs = vec!["HEAD -> main".to_string(), "origin/main".to_string()];
        assert_eq!(
            commit_tooltip(&c),
            "HEAD -> main, origin/main\nabcdef1\n\nsubject\n\nwhy it happened\nand a second line"
        );
    }

    #[test]
    fn same_head_skips_the_splice_only_where_the_page_really_is_unchanged() {
        let page: Vec<Commit> = ["c", "b", "a"].iter().map(|id| commit_at(id)).collect();
        assert!(same_head(&page, &page), "the ordinary refresh");
        assert!(same_head(&[], &[]));

        let mut loaded = page.clone();
        loaded.push(commit_at("older"));
        assert!(same_head(&loaded, &page), "a Load More survives a refresh");

        let mut newer = vec![commit_at("d")];
        newer.extend(page.clone());
        assert!(!same_head(&page, &newer), "a new commit");
        assert!(!same_head(&page, &page[1..]), "a commit taken away");
        assert!(
            !same_head(&[], &page),
            "the first refresh has nothing to keep"
        );

        let mut decorated = page.clone();
        decorated[0].refs = vec!["main".to_string()];
        assert!(!same_head(&page, &decorated), "a branch moved onto it");
    }
}
