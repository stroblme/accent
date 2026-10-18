//! The debugger's half of the Code pane: its controls in a strip, and Call Stack, Variables and
//! Breakpoints as sections, filled as the window asks the paused program.

use super::widgets::{Section, path_list};
use crate::widgets::{icon_button, scroller};
use accent_api::debug::{Frame, Scope, Thread, Variable};
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
}

/// A row of Call Stack or Variables.
enum Item {
    Thread(Thread),
    Frame(Frame),
    Scope(Scope),
    Variable(Variable),
}

impl Item {
    /// What its children are listed by, for a row that has any.
    fn key(&self) -> Option<i64> {
        match self {
            Item::Thread(thread) => Some(thread.id),
            Item::Frame(_) => None,
            Item::Scope(Scope { reference, .. }) | Item::Variable(Variable { reference, .. }) => {
                (*reference > 0).then_some(*reference)
            }
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

/// A tree row: the expander, the name, and what is said beside it, dimmed.
fn row_factory(tree: &Tree) -> gtk::SignalListItemFactory {
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
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&name);
            row.append(&detail);
            let expander = gtk::TreeExpander::new();
            expander.set_child(Some(&row));
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
            let (Some(name), Some(detail)) = (
                line.first_child().and_downcast::<gtk::Label>(),
                line.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            draw(&item.borrow::<Item>(), &line, &name, &detail);
        },
    )
}

/// A thread or a scope by its name; a call by its name, its file and line dimmed beside it and the
/// whole row dimmed outside the vault; a value as `name = value` in the editor's monospace, its
/// type in the tooltip.
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
    };
    name.set_text(&text);
    let value = matches!(item, Item::Variable(v) if !v.value.is_empty());
    crate::widgets::set_class(name, "monospace", value);
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

fn tree_view(tree: &Tree) -> gtk::ListView {
    let view = gtk::ListView::new(Some(tree.selection.clone()), Some(row_factory(tree)));
    view.add_css_class("navigation-sidebar");
    view
}

pub(super) struct Debug {
    /// The controls, and the state line under them.
    pub(super) strip: gtk::Box,
    state: gtk::Label,
    resume: gtk::Button,
    pause: gtk::Button,
    pub(super) stack: Section,
    stack_body: gtk::Stack,
    stack_line: gtk::Label,
    calls: Tree,
    pub(super) variables: Section,
    variables_body: gtk::Stack,
    values: Tree,
    pub(super) breakpoints: Section,
    breakpoints_body: gtk::Stack,
    breakpoint_rows: gtk::StringList,
    /// Set while the window selects a frame, so the selection is not taken for the reader's.
    selecting: Rc<Cell<bool>>,
}

impl Debug {
    pub(super) fn new(data: Data) -> Debug {
        let (strip, state, resume, pause) = strip();

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
        let (stack_body, stack_line) = body(&tree_view(&calls), "");
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
        let (variables_body, _) = body(&tree_view(&values), "Shown while the program is paused");
        let variables = Section::new("Variables", &variables_body);

        let breakpoint_rows = gtk::StringList::new(&[]);
        let pick = data.breakpoint.clone();
        let list = path_list(
            &breakpoint_rows,
            crate::references::reference_icon,
            move |row| pick(row),
        );
        let (breakpoints_body, _) = body(&list, "Click a line's gutter to stop there");
        let breakpoints = Section::new("Breakpoints", &breakpoints_body);

        for section in [&stack, &variables, &breakpoints] {
            section.root.set_visible(false);
        }
        Debug {
            strip,
            state,
            resume,
            pause,
            stack,
            stack_body,
            stack_line,
            calls,
            variables,
            variables_body,
            values,
            breakpoints,
            breakpoints_body,
            breakpoint_rows,
            selecting,
        }
    }

    /// Show the session: the strip saying `line`, `why` in its tooltip, Continue or Pause as the
    /// program is `paused` or not. Running, the program has nowhere to show.
    pub(super) fn set_state(&self, line: &str, why: Option<&str>, paused: bool) {
        self.strip.set_visible(true);
        for section in [&self.stack, &self.variables, &self.breakpoints] {
            section.root.set_visible(true);
        }
        self.state.set_text(line);
        self.state.set_tooltip_text(why);
        self.set_play("win.debug-continue");
        self.resume.set_visible(paused);
        self.pause.set_visible(!paused);
        if !paused {
            self.calls.set(Vec::new());
            self.stack.set_count(None);
            self.stack_line.set_text(line);
            self.stack_body.set_visible_child_name("empty");
            self.values.set(Vec::new());
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
        self.values
            .set(scopes.iter().cloned().map(Item::Scope).collect());
        self.variables_body.set_visible_child_name("list");
        if let Some(first) = scopes.iter().find(|scope| !scope.expensive) {
            let reference = first.reference;
            self.values
                .expand(|item| matches!(item, Item::Scope(s) if s.reference == reference));
        }
    }

    pub(super) fn set_breakpoints(&self, rows: &[String]) {
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        self.breakpoint_rows
            .splice(0, self.breakpoint_rows.n_items(), &rows);
        self.breakpoints.set_count(Some(rows.len()));
        self.breakpoints_body
            .set_visible_child_name(match rows.is_empty() {
                true => "empty",
                false => "list",
            });
    }

    /// No session: nothing of one is left on screen, and while the tab in front can be debugged
    /// (`debuggable`) the strip offers Start Debugging over the breakpoints.
    pub(super) fn set_idle(&self, debuggable: bool) {
        self.strip.set_visible(debuggable);
        self.state.set_text("Not running");
        self.state.set_tooltip_text(None);
        self.set_play("win.debug-start");
        self.resume.set_visible(true);
        self.pause.set_visible(false);
        self.stack.root.set_visible(false);
        self.variables.root.set_visible(false);
        self.breakpoints.root.set_visible(debuggable);
        self.calls.set(Vec::new());
        self.values.set(Vec::new());
    }

    /// Point the play button at `action`, named by its label as the strip's other buttons are.
    fn set_play(&self, action: &str) {
        let label = crate::actions::label_of(action);
        self.resume.set_action_name(Some(action));
        self.resume.set_tooltip_text(Some(label));
        self.resume
            .update_property(&[gtk::accessible::Property::Label(label)]);
    }

    /// The strip's line, the calls listed as `name file:line` and the values as `name=value`.
    #[cfg(feature = "bench")]
    pub(super) fn shown(&self) -> (String, Vec<String>, Vec<String>) {
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
        (self.state.text().to_string(), calls, values)
    }

    pub(super) fn sections(&self) -> [&Section; 3] {
        [&self.stack, &self.variables, &self.breakpoints]
    }
}

/// The controls, flat icon buttons on the window's actions, which are enabled as the session's
/// state allows: Continue or Pause (Start Debugging while no session runs), the steps, Restart
/// and Stop, then the Debug Console; and under them what the program is doing. A press leaves
/// the keyboard where it was, so stepping keeps the reader's place.
fn strip() -> (gtk::Box, gtk::Label, gtk::Button, gtk::Button) {
    let button = |icon: &str, action: &str| {
        let button = icon_button(icon, crate::actions::label_of(action));
        button.set_action_name(Some(action));
        button.set_focus_on_click(false);
        button
    };
    let resume = button("media-playback-start-symbolic", "win.debug-continue");
    let pause = button("media-playback-pause-symbolic", "win.debug-pause");
    // Centred, as the pane switcher's icons are in the header above, and the line under them too.
    let buttons = gtk::Box::builder().halign(gtk::Align::Center).build();
    for widget in [
        &resume,
        &pause,
        &button("dap-step-over-symbolic", "win.debug-step-over"),
        &button("dap-step-into-symbolic", "win.debug-step-into"),
        &button("dap-step-out-symbolic", "win.debug-step-out"),
        &button("view-refresh-symbolic", "win.debug-restart"),
        &button("media-playback-stop-symbolic", "win.debug-stop"),
        &button("dap-console-symbolic", "win.debug-console"),
    ] {
        buttons.append(widget);
    }
    let state = gtk::Label::builder()
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    state.add_css_class("caption");
    state.add_css_class("dim-label");
    let strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .visible(false)
        .build();
    strip.append(&buttons);
    strip.append(&state);
    (strip, state, resume, pause)
}
