//! The debugger's half of the Code pane: its controls in a strip, and Call Stack, Variables,
//! Watch and Breakpoints as sections, filled as the window asks the paused program.

use super::breakpoints::{Edited, Row, Rows};
use super::widgets::Section;
use crate::widgets::{icon_button, scroller};
use accent_api::debug::{Evaluated, ExceptionFilter, Frame, Scope, Supports, Thread, Variable};
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// What the sections ask of the session, as closures so the pane never sees a vault. An answer
/// is handed back once it comes, and never once the program has moved on since.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// A thread's calls, innermost first.
    pub frames: Rc<dyn Fn(i64, Box<dyn FnOnce(Vec<Frame>)>)>,
    /// What a value with a reference holds.
    pub children: Rc<dyn Fn(i64, Box<dyn FnOnce(Vec<Variable>)>)>,
    /// A frame picked in Call Stack.
    pub frame: Rc<dyn Fn(&Frame)>,
    /// A Breakpoints row activated, as its `path:line`.
    pub breakpoint: Rc<dyn Fn(&str)>,
    /// A breakpoint edited in its row.
    pub edit: Edited,
    /// An expression typed into Watch's entry, to watch.
    pub watch: Rc<dyn Fn(&str)>,
    /// A watched expression's remove button pressed.
    pub unwatch: Rc<dyn Fn(&str)>,
    /// An exception's switch flipped.
    pub exception: Flipped,
}

/// An exception's filter, and whether the program stops at it now.
pub type Flipped = Rc<dyn Fn(&str, bool)>;

/// A watched expression and, while the program is paused, its value or why it has none.
pub type Watched = (String, Option<Result<Evaluated, String>>);

/// What takes a watched expression out, for the trees whose rows can be watched expressions.
type Unwatch = Option<Rc<dyn Fn(&str)>>;

/// The strip's line, then the rows of Call Stack, Variables and Watch as a drill reads them.
#[cfg(feature = "bench")]
pub type Shown = (String, Vec<String>, Vec<String>, Vec<String>);

/// A row of Call Stack, Variables or Watch.
enum Item {
    Thread(Thread),
    Frame(Frame),
    Scope(Scope),
    Variable(Variable),
    Watch(Watched),
}

impl Item {
    /// What its children are listed by, for a row that has any.
    fn key(&self) -> Option<i64> {
        match self {
            Item::Thread(thread) => Some(thread.id),
            Item::Frame(_) => None,
            Item::Scope(Scope { reference, .. })
            | Item::Variable(Variable { reference, .. })
            | Item::Watch((_, Some(Ok(Evaluated { reference, .. })))) => {
                (*reference > 0).then_some(*reference)
            }
            Item::Watch(_) => None,
        }
    }
}

/// The item a row of `model` shows.
fn item_of(object: &glib::Object) -> Option<glib::BoxedAnyObject> {
    match object.downcast_ref::<gtk::TreeListRow>() {
        Some(row) => row.item().and_downcast(),
        None => object.clone().downcast().ok(),
    }
}

/// Lists by their keys, so the expander's probing of `create_func` for every row it binds lists
/// nothing: rows are fetched as they are opened.
type Lists = Rc<RefCell<HashMap<i64, gio::ListStore>>>;

/// What fetches a row's children into its list.
type Fetch = Rc<dyn Fn(i64, &gio::ListStore)>;

/// A lazy tree of [`Item`]s: roots the window sets, and each opened row's children fetched once.
struct Tree {
    roots: gio::ListStore,
    lists: Lists,
    asked: Rc<RefCell<HashSet<i64>>>,
    model: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    fetch: Fetch,
}

impl Tree {
    fn new(fetch: Fetch) -> Tree {
        let roots = gio::ListStore::new::<glib::BoxedAnyObject>();
        let lists: Lists = Rc::default();
        let model = gtk::TreeListModel::new(roots.clone(), false, false, {
            let lists = lists.clone();
            move |object| {
                let key = item_of(object)?.borrow::<Item>().key()?;
                let list = lists
                    .borrow_mut()
                    .entry(key)
                    .or_insert_with(gio::ListStore::new::<glib::BoxedAnyObject>)
                    .clone();
                Some(list.upcast())
            }
        });
        let selection = gtk::SingleSelection::builder()
            .model(&model)
            .autoselect(false)
            .can_unselect(true)
            .build();
        Tree {
            roots,
            lists,
            asked: Rc::default(),
            model,
            selection,
            fetch,
        }
    }

    /// The rows under `key`, fetched the first time it is opened.
    fn open(&self, key: i64) {
        open_once(&self.lists, &self.asked, &self.fetch, key);
    }

    /// Start again from `roots`, every list let go.
    fn set(&self, roots: Vec<Item>) {
        self.lists.borrow_mut().clear();
        self.asked.borrow_mut().clear();
        let objects: Vec<glib::BoxedAnyObject> =
            roots.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.roots.splice(0, self.roots.n_items(), &objects);
    }

    /// The list under `key`, filled now: what the window already had of it.
    fn fill(&self, key: i64, items: Vec<Item>) {
        self.asked.borrow_mut().insert(key);
        let list = self
            .lists
            .borrow_mut()
            .entry(key)
            .or_insert_with(gio::ListStore::new::<glib::BoxedAnyObject>)
            .clone();
        fill(&list, items);
    }

    /// The position of the first row `matches` picks.
    fn find(&self, matches: impl Fn(&Item) -> bool) -> Option<u32> {
        (0..self.model.n_items()).find(|&i| {
            self.model
                .item(i)
                .as_ref()
                .and_then(item_of)
                .is_some_and(|item| matches(&item.borrow::<Item>()))
        })
    }

    /// Open the row `matches` picks.
    fn expand(&self, matches: impl Fn(&Item) -> bool) {
        if let Some(row) = self.find(matches).and_then(|i| self.model.row(i)) {
            row.set_expanded(true);
            if let Some(key) = row
                .item()
                .and_downcast::<glib::BoxedAnyObject>()
                .and_then(|item| item.borrow::<Item>().key())
            {
                self.open(key);
            }
        }
    }
}

/// Ask for `key`'s children unless they were asked for already.
fn open_once(lists: &Lists, asked: &Rc<RefCell<HashSet<i64>>>, fetch: &Fetch, key: i64) {
    let list = lists.borrow().get(&key).cloned();
    if let Some(list) = list
        && asked.borrow_mut().insert(key)
    {
        fetch(key, &list);
    }
}

fn fill(list: &gio::ListStore, items: Vec<Item>) {
    let objects: Vec<glib::BoxedAnyObject> =
        items.into_iter().map(glib::BoxedAnyObject::new).collect();
    list.splice(0, list.n_items(), &objects);
}

/// A tree row: the expander, the name, and what is said beside it, dimmed; a watched expression
/// has a button at the end that takes it out, `unwatch`.
fn row_factory(tree: &Tree, unwatch: Unwatch) -> gtk::SignalListItemFactory {
    let (lists, asked, fetch) = (tree.lists.clone(), tree.asked.clone(), tree.fetch.clone());
    crate::widgets::factory(
        move |_| {
            let name = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .single_line_mode(true)
                .build();
            let detail = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Start)
                .build();
            detail.add_css_class("dim-label");
            let remove = icon_button("window-close-symbolic", "Remove Expression");
            remove.set_hexpand(true);
            remove.set_halign(gtk::Align::End);
            remove.set_visible(false);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&name);
            row.append(&detail);
            row.append(&remove);
            let expander = gtk::TreeExpander::new();
            expander.set_child(Some(&row));
            if let Some(unwatch) = unwatch.clone() {
                remove.connect_clicked(glib::clone!(
                    #[weak]
                    expander,
                    move |_| {
                        let item = expander.item().and_downcast::<glib::BoxedAnyObject>();
                        if let Some(Item::Watch((expression, _))) =
                            item.as_ref().map(|i| i.borrow::<Item>()).as_deref()
                        {
                            unwatch(expression);
                        }
                    }
                ));
            }
            // Rows are recycled, so the handler moves with the row the widget is bound to.
            let opened: RefCell<Option<(gtk::TreeListRow, glib::SignalHandlerId)>> =
                RefCell::default();
            let (lists, asked, fetch) = (lists.clone(), asked.clone(), fetch.clone());
            expander.connect_list_row_notify(move |expander| {
                if let Some((row, handler)) = opened.borrow_mut().take() {
                    row.disconnect(handler);
                }
                let Some(row) = expander.list_row() else {
                    return;
                };
                let (lists, asked, fetch) = (lists.clone(), asked.clone(), fetch.clone());
                let handler = row.connect_expanded_notify(move |row| {
                    let key = row
                        .item()
                        .and_downcast::<glib::BoxedAnyObject>()
                        .and_then(|item| item.borrow::<Item>().key());
                    if let Some(key) = key.filter(|_| row.is_expanded()) {
                        open_once(&lists, &asked, &fetch, key);
                    }
                });
                *opened.borrow_mut() = Some((row, handler));
            });
            expander
        },
        |expander: &gtk::TreeExpander, item| {
            let row = item.item().and_downcast::<gtk::TreeListRow>();
            expander.set_list_row(row.as_ref());
            let (Some(item), Some(line)) = (
                row.and_then(|row| row.item())
                    .and_downcast::<glib::BoxedAnyObject>(),
                expander.child().and_downcast::<gtk::Box>(),
            ) else {
                return;
            };
            let name = line.first_child().and_downcast::<gtk::Label>();
            let detail = name.as_ref().and_then(|name| name.next_sibling());
            let (Some(name), Some(detail), Some(remove)) =
                (name, detail.and_downcast::<gtk::Label>(), line.last_child())
            else {
                return;
            };
            let item = item.borrow::<Item>();
            remove.set_visible(matches!(*item, Item::Watch(_)));
            draw(&item, &line, &name, &detail);
        },
    )
}

/// A thread or a scope by its name; a call by its name, its file and line dimmed beside it and the
/// whole row dimmed outside the vault; a value, and a watched expression's, as `name = value` in
/// the editor's monospace, its type in the tooltip; an expression's error in the error colour,
/// whole in the tooltip.
fn draw(item: &Item, line: &gtk::Box, name: &gtk::Label, detail: &gtk::Label) {
    let (text, said, outside, tooltip) = match item {
        Item::Thread(thread) => (thread.name.clone(), None, false, None),
        Item::Scope(scope) => (scope.name.clone(), None, false, None),
        Item::Frame(frame) => {
            let at = frame.path.as_deref().map(|path| {
                (
                    path,
                    format!("{}:{}", crate::doc::file_name(path), frame.pos.line + 1),
                )
            });
            let outside = frame.path.as_deref().is_none_or(crate::doc::is_loose_key);
            let tooltip = at
                .as_ref()
                .map(|(path, _)| format!("{path}:{}", frame.pos.line + 1));
            (frame.name.clone(), at.map(|(_, at)| at), outside, tooltip)
        }
        // A group the adapter lists, such as debugpy's "special variables", has no value.
        Item::Variable(variable) if variable.value.is_empty() => {
            (variable.name.clone(), None, false, None)
        }
        Item::Variable(variable) => (
            format!("{} = {}", variable.name, variable.value),
            None,
            false,
            variable.kind.clone(),
        ),
        Item::Watch((expression, value)) => match value {
            None => (expression.clone(), None, false, None),
            Some(Ok(value)) => (
                format!("{expression} = {}", value.result),
                None,
                false,
                value.kind.clone(),
            ),
            Some(Err(why)) => (
                format!("{expression}: {why}"),
                None,
                false,
                Some(why.clone()),
            ),
        },
    };
    name.set_text(&text);
    let value =
        matches!(item, Item::Variable(v) if !v.value.is_empty()) || matches!(item, Item::Watch(_));
    crate::widgets::set_class(name, "monospace", value);
    crate::widgets::set_class(
        name,
        "error",
        matches!(item, Item::Watch((_, Some(Err(_))))),
    );
    detail.set_visible(said.is_some());
    detail.set_text(said.as_deref().unwrap_or_default());
    crate::widgets::set_class(line, "dim-label", outside);
    line.set_tooltip_text(tooltip.as_deref());
}

/// A section's body: its list, or the dim line said instead while it has none.
fn body(list: &impl IsA<gtk::Widget>, empty: &str) -> (gtk::Stack, gtk::Label) {
    let stack = gtk::Stack::builder().vexpand(true).build();
    let line = gtk::Label::builder()
        .label(empty)
        .xalign(0.0)
        .valign(gtk::Align::Start)
        .wrap(true)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    line.add_css_class("dim-label");
    stack.add_named(&line, Some("empty"));
    stack.add_named(&scroller(list), Some("list"));
    (stack, line)
}

fn tree_view(tree: &Tree, unwatch: Unwatch) -> gtk::ListView {
    let factory = row_factory(tree, unwatch);
    let view = gtk::ListView::new(Some(tree.selection.clone()), Some(factory));
    view.add_css_class("navigation-sidebar");
    view
}

pub(super) struct Debug {
    /// The controls, and the state line under them.
    pub(super) strip: gtk::Box,
    state: Caption,
    controls: Controls,
    pub(super) stack: Section,
    stack_body: gtk::Stack,
    stack_line: gtk::Label,
    calls: Tree,
    pub(super) variables: Section,
    variables_body: gtk::Stack,
    values: Tree,
    /// The Debug Console's last answer that holds more, atop Variables while the program stays
    /// paused.
    answer: RefCell<Option<Variable>>,
    pub(super) watch: Section,
    watched: Tree,
    pub(super) breakpoints: Section,
    breakpoints_body: gtk::Stack,
    breakpoint_rows: Rc<Rows>,
    /// The exception switches atop Breakpoints, and the filters they show.
    exception_rows: gtk::Box,
    exceptions: RefCell<Vec<ExceptionFilter>>,
    exception: Flipped,
    /// Set while the window selects a frame, so the selection is not taken for the reader's.
    selecting: Rc<Cell<bool>>,
}

impl Debug {
    pub(super) fn new(data: Data) -> Debug {
        let (strip, state, controls) = strip();

        let frames = data.frames.clone();
        let calls = Tree::new(Rc::new(move |thread, list| {
            let list = list.downgrade();
            frames(
                thread,
                Box::new(move |frames| {
                    if let Some(list) = list.upgrade() {
                        fill(&list, frames.into_iter().map(Item::Frame).collect());
                    }
                }),
            );
        }));
        let selecting: Rc<Cell<bool>> = Rc::default();
        // A single click picks a call, as a result list's opens its row; the window selecting
        // the paused one is not a pick.
        calls.selection.connect_selected_item_notify({
            let (pick, selecting) = (data.frame.clone(), selecting.clone());
            move |selection| {
                let item = selection.selected_item().as_ref().and_then(item_of);
                if let Some(Item::Frame(frame)) =
                    item.as_ref().map(|i| i.borrow::<Item>()).as_deref()
                    && !selecting.get()
                {
                    pick(frame);
                }
            }
        });
        let (stack_body, stack_line) = body(&tree_view(&calls, None), "");
        let stack = Section::new("Call Stack", &stack_body);

        let children = data.children.clone();
        let values = Tree::new(Rc::new(move |reference, list| {
            let list = list.downgrade();
            children(
                reference,
                Box::new(move |variables| {
                    if let Some(list) = list.upgrade() {
                        fill(&list, variables.into_iter().map(Item::Variable).collect());
                    }
                }),
            );
        }));
        let (variables_body, _) = body(
            &tree_view(&values, None),
            "Shown while the program is paused",
        );
        let variables = Section::new("Variables", &variables_body);

        let children = data.children.clone();
        let watched = Tree::new(Rc::new(move |reference, list| {
            let list = list.downgrade();
            children(
                reference,
                Box::new(move |variables| {
                    if let Some(list) = list.upgrade() {
                        fill(&list, variables.into_iter().map(Item::Variable).collect());
                    }
                }),
            );
        }));
        let watch = Section::new("Watch", &watch_body(&watched, &data));

        let breakpoint_rows = Rows::new(data.breakpoint.clone(), data.edit.clone());
        let (breakpoints_body, _) =
            body(&breakpoint_rows.list, "Click a line's gutter to stop there");
        let exception_rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let breakpoints_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        breakpoints_page.append(&exception_rows);
        breakpoints_page.append(&breakpoints_body);
        let breakpoints = Section::new("Breakpoints", &breakpoints_page);

        for section in [&stack, &variables, &watch, &breakpoints] {
            section.root.set_visible(false);
        }
        Debug {
            strip,
            state,
            controls,
            stack,
            stack_body,
            stack_line,
            calls,
            variables,
            variables_body,
            values,
            answer: RefCell::default(),
            watch,
            watched,
            breakpoints,
            breakpoints_body,
            breakpoint_rows,
            exception_rows,
            exceptions: RefCell::default(),
            exception: data.exception.clone(),
            selecting,
        }
    }

    /// Show the session: the strip saying `line`, `why` in its tooltip, Continue or Pause as the
    /// program is `paused` or not. Running, the program has nowhere to show.
    pub(super) fn set_state(&self, line: &str, why: Option<&str>, paused: bool) {
        self.strip.set_visible(true);
        for section in self.sections() {
            section.root.set_visible(true);
        }
        // Starting, running or stopping, the program is at work until it pauses.
        self.state.say(line, why, !paused);
        self.controls.session.set_visible(true);
        self.controls.start.set_visible(false);
        self.controls.resume.set_visible(paused);
        self.controls.pause.set_visible(!paused);
        if !paused {
            self.calls.set(Vec::new());
            self.stack.set_count(None);
            self.stack_line.set_text(line);
            self.stack_body.set_visible_child_name("empty");
            self.values.set(Vec::new());
            self.answer.replace(None);
            self.variables_body.set_visible_child_name("empty");
        }
    }

    /// The threads, and the paused one open on its calls.
    pub(super) fn set_threads(&self, threads: &[Thread], paused: Option<(i64, Vec<Frame>)>) {
        self.calls
            .set(threads.iter().cloned().map(Item::Thread).collect());
        self.stack.set_count(Some(threads.len()));
        self.stack_body.set_visible_child_name("list");
        if let Some((thread, frames)) = paused {
            self.calls
                .fill(thread, frames.into_iter().map(Item::Frame).collect());
            self.calls
                .expand(|item| matches!(item, Item::Thread(t) if t.id == thread));
        }
    }

    pub(super) fn select_frame(&self, id: i64) {
        if let Some(at) = self
            .calls
            .find(|item| matches!(item, Item::Frame(f) if f.id == id))
        {
            self.selecting.set(true);
            self.calls.selection.set_selected(at);
            self.selecting.set(false);
        }
    }

    /// The selected frame's scopes, the first that is cheap to list open.
    pub(super) fn set_scopes(&self, scopes: &[Scope]) {
        let answer = self.answer.borrow().clone().map(Item::Variable);
        let rows = answer
            .into_iter()
            .chain(scopes.iter().cloned().map(Item::Scope));
        self.values.set(rows.collect());
        self.variables_body.set_visible_child_name("list");
        if let Some(first) = scopes.iter().find(|scope| !scope.expensive) {
            let reference = first.reference;
            self.values
                .expand(|item| matches!(item, Item::Scope(s) if s.reference == reference));
        }
    }

    /// The Debug Console's last answer that holds more, first in Variables to be opened as a value
    /// is, in place of the one before; the rows below keep what they had open.
    pub(super) fn set_answer(&self, answer: Variable) {
        if self.answer.replace(Some(answer.clone())).is_some() {
            self.values.roots.remove(0);
        }
        let row = glib::BoxedAnyObject::new(Item::Variable(answer));
        self.values.roots.insert(0, &row);
        self.variables_body.set_visible_child_name("list");
    }

    /// Open the console's answer atop Variables, as a reader would.
    #[cfg(feature = "bench")]
    pub(super) fn open_answer(&self) {
        let answer = self.answer.borrow().clone();
        self.values.expand(|item| match (item, &answer) {
            (Item::Variable(v), Some(answer)) => v.reference == answer.reference,
            _ => false,
        });
    }

    /// The watched expressions, with their values while the program is paused.
    pub(super) fn set_watches(&self, rows: &[Watched]) {
        self.watched
            .set(rows.iter().cloned().map(Item::Watch).collect());
        self.watch.set_count(Some(rows.len()));
    }

    /// A switch for each exception the program can stop at, atop the breakpoints, made again only
    /// when the filters have changed.
    pub(super) fn set_exceptions(&self, filters: &[ExceptionFilter]) {
        if *self.exceptions.borrow() == filters {
            return;
        }
        self.exceptions.replace(filters.to_vec());
        while let Some(row) = self.exception_rows.first_child() {
            self.exception_rows.remove(&row);
        }
        for filter in filters {
            let switch = gtk::CheckButton::builder()
                .label(&filter.label)
                .active(filter.on)
                .margin_start(10)
                .margin_end(6)
                .build();
            switch.set_tooltip_text(filter.description.as_deref());
            let (exception, id) = (self.exception.clone(), filter.filter.clone());
            switch.connect_toggled(move |switch| exception(&id, switch.is_active()));
            self.exception_rows.append(&switch);
        }
    }

    pub(super) fn set_breakpoints(&self, rows: &[Row], supports: Supports) {
        self.breakpoint_rows.set(rows, supports);
        self.breakpoints.set_count(Some(rows.len()));
        self.breakpoints_body
            .set_visible_child_name(match rows.is_empty() {
                true => "empty",
                false => "list",
            });
    }

    /// Whether the strip shows a session's controls rather than Start Debugging.
    pub(super) fn in_session(&self) -> bool {
        self.controls.session.get_visible()
    }

    /// No session: nothing of one is left on screen, and while the tab in front can be debugged
    /// (`debuggable`) the strip offers Start Debugging over the breakpoints.
    pub(super) fn set_idle(&self, debuggable: bool) {
        self.strip.set_visible(debuggable);
        self.state.say("Not running", None, false);
        self.controls.session.set_visible(false);
        self.controls.start.set_visible(true);
        self.stack.root.set_visible(false);
        self.variables.root.set_visible(false);
        self.watch.root.set_visible(debuggable);
        self.breakpoints.root.set_visible(debuggable);
        self.calls.set(Vec::new());
        self.values.set(Vec::new());
        self.answer.replace(None);
    }

    /// The strip's line, the calls listed as `name file:line`, the values as `name=value` and the
    /// watched expressions as their rows read.
    #[cfg(feature = "bench")]
    pub(super) fn shown(&self) -> Shown {
        let rows = |tree: &Tree, said: &dyn Fn(&Item) -> Option<String>| -> Vec<String> {
            (0..tree.model.n_items())
                .filter_map(|i| tree.model.item(i).as_ref().and_then(item_of))
                .filter_map(|item| said(&item.borrow::<Item>()))
                .collect()
        };
        let calls = rows(&self.calls, &|item| match item {
            Item::Frame(frame) => Some(format!(
                "{} {}:{}",
                frame.name,
                frame.path.as_deref().map_or("", crate::doc::file_name),
                frame.pos.line + 1
            )),
            _ => None,
        });
        let values = rows(&self.values, &|item| match item {
            Item::Variable(v) if !v.value.is_empty() => Some(format!("{}={}", v.name, v.value)),
            _ => None,
        });
        let watched = rows(&self.watched, &|item| match item {
            Item::Watch((expression, Some(Ok(value)))) => {
                Some(format!("{expression} = {}", value.result))
            }
            Item::Watch((expression, _)) => Some(expression.clone()),
            _ => None,
        });
        (self.state.text(), calls, values, watched)
    }

    #[cfg(feature = "bench")]
    pub(super) fn breakpoint_rows(&self) -> &Rc<Rows> {
        &self.breakpoint_rows
    }

    pub(super) fn sections(&self) -> [&Section; 4] {
        [&self.stack, &self.variables, &self.watch, &self.breakpoints]
    }
}

/// Watch's body: its rows, and under them the entry an expression is added from.
fn watch_body(watched: &Tree, data: &Data) -> gtk::Box {
    let entry = gtk::Entry::builder()
        .placeholder_text("Add Expression")
        .margin_start(6)
        .margin_end(6)
        .margin_top(3)
        .margin_bottom(6)
        .build();
    let watch = data.watch.clone();
    entry.connect_activate(move |entry| {
        let expression = entry.text().trim().to_string();
        if !expression.is_empty() {
            entry.set_text("");
            watch(&expression);
        }
    });
    let list = scroller(&tree_view(watched, Some(data.unwatch.clone())));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&list);
    body.append(&entry);
    body
}

/// What the strip shows of the session's controls: the session's own while one runs, Start
/// Debugging while none does.
struct Controls {
    /// Continue or Pause, the steps, Restart and Stop.
    session: gtk::Box,
    resume: gtk::Button,
    pause: gtk::Button,
    start: gtk::Button,
}

/// The controls, flat icon buttons on the window's actions, which are enabled as the session's
/// state allows: Continue or Pause, the steps, Restart and Stop, then the Debug Console, and
/// Start Debugging in their place while no session runs; and under them what the program is
/// doing.
fn strip() -> (gtk::Box, Caption, Controls) {
    let resume = strip_button("media-playback-start-symbolic", "win.debug-continue");
    let pause = strip_button("media-playback-pause-symbolic", "win.debug-pause");
    let session = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for button in [
        &resume,
        &pause,
        &strip_button("dap-step-over-symbolic", "win.debug-step-over"),
        &strip_button("dap-step-into-symbolic", "win.debug-step-into"),
        &strip_button("dap-step-out-symbolic", "win.debug-step-out"),
        &strip_button("view-refresh-symbolic", "win.debug-restart"),
        &strip_button("media-playback-stop-symbolic", "win.debug-stop"),
    ] {
        session.append(button);
    }
    // The Git pane's Commit: a text button in the accent colour, one word as Commit is, after
    // the Debug Console and 6 px from it as Commit is from Sync. The greyed steps it replaces had
    // nothing to offer before a program runs.
    let start = gtk::Button::builder()
        .label("Debug")
        .tooltip_text(crate::actions::label_of("win.debug-start"))
        .action_name("win.debug-start")
        .focus_on_click(false)
        .margin_start(6)
        // At its own height, as the icon buttons are, rather than stretched to a taller row.
        .valign(gtk::Align::Center)
        .build();
    start.add_css_class("suggested-action");
    let (strip, state) = strip_of(&[
        session.clone().upcast(),
        strip_button("dap-console-symbolic", "win.debug-console").upcast(),
        start.clone().upcast(),
    ]);
    let controls = Controls {
        session,
        resume,
        pause,
        start,
    };
    (strip, state, controls)
}

/// A strip's flat icon button on the window's `action`, named by its label. A press leaves the
/// keyboard where it was, so stepping keeps the reader's place.
pub(super) fn strip_button(icon: &str, action: &str) -> gtk::Button {
    let button = icon_button(icon, crate::actions::label_of(action));
    button.set_action_name(Some(action));
    button.set_focus_on_click(false);
    button
}

/// A strip of `buttons` over a dim caption, hidden until it is shown: the debugger's, and the
/// notebook kernel's.
pub(super) fn strip_of(buttons: &[gtk::Widget]) -> (gtk::Box, Caption) {
    // Centred, as the pane switcher's icons are in the header above.
    let row = gtk::Box::builder().halign(gtk::Align::Center).build();
    for button in buttons {
        row.append(button);
    }
    let state = Caption::new();
    let strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .visible(false)
        .build();
    strip.append(&row);
    strip.append(&state.root);
    (strip, state)
}

/// A strip's dim caption: what the program or kernel does, beside a spinner while it works, the
/// indicator the Git pane's Sync shows while it syncs.
pub(super) struct Caption {
    root: gtk::Box,
    line: gtk::Label,
    spinner: adw::Spinner,
}

impl Caption {
    fn new() -> Caption {
        let line = gtk::Label::builder()
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        // Not dimmed with the words: what is at work should catch the eye, as Sync's does.
        let spinner = adw::Spinner::builder().visible(false).build();
        line.add_css_class("caption");
        line.add_css_class("dim-label");
        // Centred, as the switcher's icons are, and as tall as the spinner with it or without,
        // so the sections under it stay put as it comes and goes.
        let root = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .height_request(16)
            .build();
        root.append(&spinner);
        root.append(&line);
        Caption {
            root,
            line,
            spinner,
        }
    }

    /// Say `line`, `why` in its tooltip, the spinner up while `working`.
    pub(super) fn say(&self, line: &str, why: Option<&str>, working: bool) {
        self.line.set_text(line);
        self.line.set_tooltip_text(why);
        self.spinner.set_visible(working);
    }

    /// What it says, for the drills.
    #[cfg(feature = "bench")]
    pub(super) fn text(&self) -> String {
        self.line.text().to_string()
    }
}
