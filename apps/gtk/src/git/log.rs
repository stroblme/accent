//! The history: the commit rows, the graph beside them, and what a commit expands into.

use super::compare::Sides;
use super::*;

/// The width of one graph lane, in px.
const LANE: i32 = 12;

/// How far a commit the remote has and HEAD does not is faded. Enough to read as "this is not
/// here yet" beside a commit that is, and not so far that the summary stops being legible.
const NOT_PULLED_DIM: f64 = 0.55;

/// How many of a commit's decorations get a label of their own; the rest are a `+N`.
const SHOWN_REFS: usize = 3;

/// One line of the history list. A flat store with two kinds rather than a `GtkTreeListModel`:
/// the log is spliced wholesale on every refresh anyway, so a tree model would only add a
/// create-child-model closure and a placeholder state to keep in step with it.
// Every item already lives on the heap inside its `BoxedAnyObject`, and the rows that are not
// commits are a handful at a time, so the size of the commit variant costs nothing a second box
// would save.
#[allow(clippy::large_enum_variant)]
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
        let (setup, bound) = (Rc::downgrade(self), Rc::downgrade(self));
        view.set_factory(Some(&crate::widgets::factory(
            move |item| log_row(item, &setup),
            // The row is rebuilt from the item rather than from the stack handed over: a
            // recycled row draws what it is bound to, not what it held.
            move |_: &gtk::Stack, item| bind_log(item, &bound),
        )));

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
            let files = crate::work::attempt("list the commit's files", move || {
                vault.git_changed_files(&repo, &query)
            })
            .await;
            let files = match files {
                Ok(files) => files,
                Err(why) => return (panel.hooks.toast)(&why),
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
            let page = crate::work::attempt("load more history", move || {
                vault.git_log(&repo, skip, PAGE)
            })
            .await;
            let page = match page {
                Ok(page) => page,
                // Whatever went wrong, the history behind the row is still there, so the Load
                // More row comes back rather than the list ending on a failure nobody saw.
                Err(why) => {
                    (panel.hooks.toast)(&why);
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

    /// Put a commit's id on the clipboard. No toast for the clipboard alone would be truer to
    /// DESIGN.md, but nothing else on screen says the id was taken: the row looks the same either
    /// way.
    fn copy_id(&self, oid: &str) {
        self.hooks.window.clipboard().set_text(oid);
        (self.hooks.toast)(&format!("Copied {}", short(oid)));
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

    // A label per decoration, the first [`SHOWN_REFS`] and then a count of the rest, because a
    // release commit can carry a handful of tags and the summary beside them still has to be
    // read. Each is ellipsized like every other name in the pane — a decoration is as long as the
    // branch it names, and without this a long branch is the sidebar's floor — and capped, the
    // tooltip having every name whole.
    let refs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    for _ in 0..=SHOWN_REFS {
        refs.append(
            &gtk::Label::builder()
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(16)
                .build(),
        );
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
    // "side branched here", on the commit where the lanes beside this one end. Not dimmed, so it
    // does not read as more of the author line above it.
    let forked = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    forked.add_css_class("caption");

    // The row's own vertical margin is off, so the breathing room lives here (see the `.git-log`
    // rule): the drawing area has to reach the row's edges for the lanes to join.
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    text.append(&line);
    text.append(&meta);
    text.append(&forked);

    let commit = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    commit.append(&area);
    commit.append(&text);
    commit.append(&commit_actions(item, panel));

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

    stack
}

/// A commit row's Check Out Commit and Copy Commit ID buttons, the two things a commit offers.
///
/// The same surface a changed file's actions have: hidden until the pointer or the keyboard is on
/// the row (`.git-actions` and `changes::reveal_on_hover`), in a revealer so they measure nothing
/// while they are away and the summary reads out to the whole width of the pane. Like those, they
/// hold the `GtkListItem` rather than the row's data, because the data under a recycled row is
/// replaced without the widgets being rebuilt.
fn commit_actions(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Revealer {
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.add_css_class("git-actions");
    for (icon, tooltip, detach) in [
        ("go-jump-symbolic", "Check Out Commit", true),
        ("edit-copy-symbolic", "Copy Commit ID", false),
    ] {
        let button = icon_button(icon, tooltip);
        let weak = panel.clone();
        button.connect_clicked(glib::clone!(
            #[weak]
            item,
            move |_| {
                let (Some(panel), Some(LogItem::Commit(row))) =
                    (weak.upgrade(), boxed(item.item()))
                else {
                    return;
                };
                match detach {
                    true => panel.detach(row.commit.id.clone()),
                    false => panel.copy_id(&row.commit.id),
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
    // The list row itself, which only exists once the item is first bound, and from an idle for
    // the reason `changes::bind_change` gives.
    let list_row = stack.parent();
    glib::idle_add_local_once(move || {
        if let Some(list_row) = list_row {
            changes::reveal_on_hover(&list_row);
        }
    });

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
    // Next sibling and not `last_child`: the row's action buttons sit after the text.
    let Some(area) = commit.first_child().and_downcast::<gtk::DrawingArea>() else {
        return;
    };
    let Some(text) = area.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(line) = text.first_child().and_downcast::<gtk::Box>() else {
        return;
    };
    let (Some(meta), Some(forked)) = (
        line.next_sibling().and_downcast::<gtk::Label>(),
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
    let Some(refs) = not_pulled.next_sibling().and_downcast::<gtk::Box>() else {
        return;
    };

    area.set_content_width(lane_width(&row));
    area.queue_draw();
    refs.set_visible(!row.commit.refs.is_empty());
    bind_refs(&refs, &row.commit.refs);
    summary.set_text(&row.commit.summary);
    meta.set_text(&format!(
        "{} · {}",
        row.commit.author,
        ago(now(), row.commit.time)
    ));
    forked.set_visible(!row.forks.is_empty());
    forked.set_text(&format!("{} branched here", row.forks.join(", ")));
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
        true => format!("Not pulled yet\n\n{}", commit_tooltip(&row)),
        false => commit_tooltip(&row),
    }));
}

/// Put a commit's decorations on the labels [`log_row`] made: the first [`SHOWN_REFS`] as names,
/// then `+N` for the rest on the last one, and any label left over hidden.
///
/// The classes are the `.git-ref` rules in `install_chrome_css`: HEAD's branch in the accent
/// colour, a remote branch dimmer than a local one, a tag outlined. Set whole on every bind,
/// because a recycled row still wears whatever the commit before it was.
fn bind_refs(labels: &gtk::Box, refs: &[git::Ref]) {
    let mut next = labels.first_child().and_downcast::<gtk::Label>();
    for i in 0..=SHOWN_REFS {
        let Some(label) = next else {
            return;
        };
        next = label.next_sibling().and_downcast();
        let (text, classes): (String, &[&str]) = match refs.get(i) {
            Some(r) if i < SHOWN_REFS => (
                r.name.clone(),
                match (r.head, r.kind) {
                    (true, _) => &["caption", "git-ref", "head"],
                    (_, git::RefKind::RemoteBranch) => &["caption", "git-ref", "remote"],
                    (_, git::RefKind::Tag) => &["caption", "git-ref", "tag"],
                    _ => &["caption", "git-ref"],
                },
            ),
            Some(_) => (
                format!("+{}", refs.len() - SHOWN_REFS),
                &["caption", "dim-label"],
            ),
            None => (String::new(), &[]),
        };
        label.set_visible(!text.is_empty());
        label.set_text(&text);
        label.set_css_classes(classes);
    }
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
/// a commit that is no branch tip simply has no first line; the branch its lane draws
/// ([`git::lanes`]) is the "On" line, where a branch or a tag above it on that lane names one.
fn commit_tooltip(row: &LogRow) -> String {
    let c = &row.commit;
    let mut head = match c.refs.is_empty() {
        true => short(&c.id),
        false => format!("{}\n{}", decorations(&c.refs), short(&c.id)),
    };
    if let Some(lane) = &row.lane {
        head = format!("{head}\nOn {lane}");
    }
    let message = match c.body.is_empty() {
        true => c.summary.clone(),
        false => format!("{}\n\n{}", c.summary, c.body),
    };
    format!("{head}\n\n{message}")
}

/// The decorations as `git log` words them: `HEAD -> main, origin/main, tag: v1`.
fn decorations(refs: &[git::Ref]) -> String {
    refs.iter()
        .map(|r| match (r.kind, r.head) {
            (git::RefKind::Tag, _) => format!("tag: {}", r.name),
            (git::RefKind::LocalBranch, true) => format!("HEAD -> {}", r.name),
            _ => r.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
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

    /// `commit` on the graph, in a column that draws `lane`.
    fn placed(commit: &Commit, lane: Option<&str>) -> LogRow {
        LogRow {
            commit: commit.clone(),
            column: 0,
            above: Vec::new(),
            below: Vec::new(),
            through: Vec::new(),
            lane: lane.map(str::to_string),
            forks: Vec::new(),
        }
    }

    #[test]
    fn commit_tooltip_says_where_the_commit_is_and_what_it_says() {
        let mut c = commit_at("abcdef1234567");
        c.summary = "subject".to_string();
        assert_eq!(commit_tooltip(&placed(&c, None)), "abcdef1\n\nsubject");

        c.body = "why it happened\nand a second line".to_string();
        assert_eq!(
            commit_tooltip(&placed(&c, Some("side"))),
            "abcdef1\nOn side\n\nsubject\n\nwhy it happened\nand a second line"
        );

        c.refs = vec![
            ref_to("main", git::RefKind::LocalBranch, true),
            ref_to("origin/main", git::RefKind::RemoteBranch, false),
            ref_to("v1", git::RefKind::Tag, false),
        ];
        assert_eq!(
            commit_tooltip(&placed(&c, Some("main"))),
            "HEAD -> main, origin/main, tag: v1\nabcdef1\nOn main\n\nsubject\n\nwhy it happened\nand a second line"
        );
    }

    fn ref_to(name: &str, kind: git::RefKind, head: bool) -> git::Ref {
        git::Ref {
            name: name.to_string(),
            kind,
            head,
        }
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
        decorated[0].refs = vec![ref_to("main", git::RefKind::LocalBranch, false)];
        assert!(!same_head(&page, &decorated), "a branch moved onto it");
    }
}
