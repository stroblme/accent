//! The Graph tab (DESIGN.md, Graph): the file in front and the files it links to or is linked
//! from, on a canvas of our own ([`view`]), a click opening a file beside the graph.
//!
//! The vault answers which files and links to show (`Vault::graph_around`) on a worker, and the
//! tab diffs each answer against what it shows ([`model`]), so a node that stays keeps its place
//! and the force layout ([`worker`]) only makes room for what is new. A click grows the graph by
//! the file clicked: it is one of the roots the vault is asked around from then on.
//! One graph a window, keyed [`KEY`]: like a preview tab it is no file.

mod camera;
mod model;
mod raster;
mod view;
mod worker;

use super::*;
use accent_core::config::GraphView as Saved;
use accent_core::graph::{NodeKind, Part, Shown};
use view::GraphView;

/// The graph tab's key, which names no file: what the session keeps it by.
pub(crate) const KEY: &str = "graph:";

/// The tab's icon, and its empty page's: tabler's `chart-dots-3`, the theme having no graph.
const ICON: &str = "links-graph-symbolic";

/// The empty page's name in the tab's holder.
const EMPTY: &str = "empty";

/// How long after the last change in the vault the graph asks again (DESIGN.md, Motion): a burst
/// of saves, a sync or a folder moved, costs one question.
const REFRESH: Duration = Duration::from_millis(300);

pub(crate) fn is_key(key: &str) -> bool {
    key == KEY
}

/// The graph's menu: Show Whole Vault; what is shown, Show Orphans while the whole vault is;
/// Reset Graph. A toggle is drawn as a check, its action carrying its state.
fn graph_menu(all: bool) -> gio::Menu {
    let section = |actions: &[&str]| {
        let section = gio::Menu::new();
        for action in actions {
            section.append(Some(crate::actions::label_of(action)), Some(action));
        }
        section
    };
    let mut shown = vec!["win.graph-unresolved", "win.graph-tags", "win.graph-images"];
    if all {
        shown.push("win.graph-orphans");
    }
    let menu = gio::Menu::new();
    menu.append_section(None, &section(&["win.graph-all"]));
    menu.append_section(None, &section(&shown));
    menu.append_section(None, &section(&["win.graph-reset"]));
    menu
}

/// What a question to the vault is for, which says how its answer is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ask {
    /// A part of the vault shown afresh, laid out cold: the first answer, Reset Graph, the whole
    /// vault turned on or off, and the graph started again around another file.
    Fresh,
    /// The roots grown by this key: what is new slides out of its node.
    Grow(String),
    /// The same roots asked again, the vault changed or something more or less shown: every
    /// node stays where it is.
    Again,
}

pub struct GraphTab {
    pub page: adw::TabPage,
    /// The page's child: blank, then loading, then the canvas, the empty page or a failure.
    holder: gtk::Stack,
    view: GraphView,
    /// The whole vault rather than the roots and their neighbours.
    all: Cell<bool>,
    shown: Cell<Shown>,
    /// The vault's answer on its way, which the next question stops.
    query: RefCell<Option<glib::JoinHandle<()>>>,
    /// Counts the questions asked, so only the last one's answer lands.
    asked: Cell<u64>,
    /// Whether it has been seeded, which waits until it is first on screen.
    started: Cell<bool>,
    /// The question asked again after the vault changed ([`REFRESH`]).
    refresh: widgets::Debounce,
    /// The last question whose answer landed.
    answered: Cell<u64>,
}

impl GraphTab {
    /// What takes the keyboard: the canvas, or the page standing in for it.
    pub fn key_target(&self) -> gtk::Widget {
        match loading::shown(&self.holder) {
            true => self.view.clone().upcast(),
            false => (self.holder.visible_child()).unwrap_or_else(|| self.holder.clone().upcast()),
        }
    }

    /// Whether the graph shows `key`'s node.
    pub fn holds(&self, key: &str) -> bool {
        self.view.graph().index_of(key).is_some()
    }

    pub fn zoom_step(&self, out: bool) {
        self.view.zoom_step(out);
    }

    pub fn fit(&self) {
        self.view.fit();
    }

    pub fn zoom_label(&self) -> String {
        self.view.zoom_label()
    }

    /// The status bar's "42 files · 57 links".
    pub fn facts(&self) -> String {
        self.view.graph().facts()
    }

    /// What the session keeps of it: what its menu shows.
    pub fn saved(&self) -> Saved {
        Saved {
            all: self.all.get(),
            shown: self.shown.get(),
        }
    }

    /// Whether it shows the whole vault, under which Show Orphans applies.
    pub fn all(&self) -> bool {
        self.all.get()
    }

    /// Show the empty page: no file to start from, and not the whole vault.
    fn show_empty(&self) {
        let none = self.view.graph().changes(&Part::default(), None);
        self.view.apply(&none);
        self.holder.set_visible_child_name(EMPTY);
    }

    /// Say what the graph is around: the tab's tooltip, and what a screen reader hears.
    fn describe(&self) {
        let tooltip = match (self.all.get(), self.view.graph().roots.first()) {
            (true, _) => "Every link in the vault".to_string(),
            (false, Some(root)) => format!("Links around {}", doc::file_name(root)),
            (false, None) => "Graph".to_string(),
        };
        self.page.set_tooltip(&tooltip);
        let facts = self.facts().replace(" · ", ", ");
        self.view
            .update_property(&[gtk::accessible::Property::Description(&facts)]);
    }
}

#[cfg(feature = "bench")]
impl GraphTab {
    /// The nodes' keys, sorted, a ghost's ending in `?`.
    pub fn nodes(&self) -> Vec<String> {
        let mut keys: Vec<String> = (self.view.graph().nodes.iter())
            .map(|n| match n.kind {
                accent_core::graph::NodeKind::Missing => format!("{}?", n.key),
                _ => n.key.clone(),
            })
            .collect();
        keys.sort();
        keys
    }

    pub fn links(&self) -> usize {
        self.view.graph().edges.len()
    }

    pub fn roots(&self) -> Vec<String> {
        self.view.graph().roots.clone()
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.view.graph().index_of(key)
    }

    /// The file drawn in the accent.
    pub fn front(&self) -> Option<String> {
        self.view.front()
    }

    /// A press on `key`'s node, as a click lands one.
    pub fn press(&self, key: &str, keep: bool) {
        let at = self.view.graph().index_of(key);
        if let Some(i) = at {
            self.view.press(i, keep);
        }
    }

    /// Whether it shows the vault's last answer.
    pub fn landed(&self) -> bool {
        loading::shown(&self.holder) && self.answered.get() == self.asked.get()
    }

    /// Whether it shows the vault's last answer, its layout settled.
    pub fn laid_out(&self) -> bool {
        self.landed() && self.view.settled()
    }

    /// The layout's frames landed since the last change.
    pub fn frames(&self) -> usize {
        self.view.frames()
    }

    pub fn worker_life(&self) -> Option<std::sync::Weak<impl Sized + use<>>> {
        self.view.worker_life()
    }
}

impl App {
    /// Show Graph (`Ctrl+Shift+M`): from the graph back to the file in front, as Toggle Preview
    /// goes back to its note; else to the graph, opened right of the tab in front if there is
    /// none, and started again around the file in front where it does not hold that.
    pub(crate) fn show_graph(self: &Rc<Self>) {
        if self.vault().is_none() {
            return self.needs_vault("show the graph");
        }
        if matches!(self.active_doc(), Some(Doc::Graph(_))) {
            if let Some((page, _)) = self.graph_front() {
                self.reveal_page(&page);
                if let Some(pane) = self.pane_of(&page) {
                    self.focus_document(&pane);
                }
            }
            return;
        }
        let graph = match self.graphs().into_iter().next() {
            Some(graph) => {
                let front = self.graph_front().map(|(_, key)| key);
                if let Some(front) = front.filter(|key| graph.started.get() && !graph.holds(key)) {
                    graph.all.set(false);
                    graph.view.graph_mut().roots = vec![front];
                    self.query_graph(&graph, Ask::Fresh);
                }
                graph
            }
            None => self.open_graph(Opened::Kept),
        };
        self.reveal_page(&graph.page);
        if let Some(pane) = self.pane_of(&graph.page) {
            self.focus_document(&pane);
        }
    }

    /// Open the graph's tab right of the tab in front, in its pane, as Toggle Preview opens a
    /// preview; a restored one goes where the session put it. It is seeded once on screen.
    pub(crate) fn open_graph(self: &Rc<Self>, how: Opened) -> Rc<GraphTab> {
        let view = GraphView::new();
        // In a box its menu hangs from: a popover on the canvas itself would be allocated by a
        // widget with no layout manager (DESIGN.md, States).
        let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
        host.append(&view);
        let holder = loading::holder();
        loading::put(&holder, &host);
        let empty = widgets::status_page(
            ICON,
            "No File Open",
            "Open a file to see what it links to, or show the whole vault.",
        );
        let all = gtk::Button::builder()
            .label("Show Whole Vault")
            .action_name("win.graph-all")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        empty.set_child(Some(&all));
        holder.add_named(&empty, Some(EMPTY));
        let page = match how {
            Opened::Restored => self.tabs_for(KEY).append(&holder),
            _ => {
                let pane = self.pane();
                let pages = pane.pages();
                let selected = pane.tabs.selected_page();
                let at = pages.iter().position(|p| Some(p) == selected.as_ref());
                let pinned = pages.iter().filter(|p| self.is_pinned(p)).count();
                let at = at.map_or(pages.len(), |at| rendered::beside(at, pinned));
                pane.tabs.insert(&holder, at as i32)
            }
        };
        page.set_title("Graph");
        page.set_icon(Some(&gio::ThemedIcon::new(ICON)));
        let graph = Rc::new(GraphTab {
            page,
            holder,
            view,
            all: Cell::new(false),
            shown: Cell::new(Shown::default()),
            query: RefCell::new(None),
            asked: Cell::new(0),
            started: Cell::new(false),
            refresh: widgets::Debounce::new(REFRESH),
            answered: Cell::new(0),
        });
        graph.describe();
        let weak = Rc::downgrade(&graph);
        graph.view.connect_open(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |i, keep| {
                if let Some(graph) = weak.upgrade() {
                    app.graph_activate(&graph, i, keep);
                }
            }
        ));
        graph.view.connect_zoomed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || app.refresh_zoom()
        ));
        // Its menu, anywhere on the canvas, built at each press: Show Orphans is the whole
        // vault's alone.
        let menu = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let weak = Rc::downgrade(&graph);
        menu.connect_pressed(move |menu, _, x, y| {
            let (Some(graph), Some(host)) = (weak.upgrade(), menu.widget()) else {
                return;
            };
            menu.set_state(gtk::EventSequenceState::Claimed);
            let anchor = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            widgets::popup_menu(&host, &graph_menu(graph.all.get()), Some(anchor));
        });
        host.add_controller(menu);
        graph.holder.connect_map(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            graph,
            move |_| app.start_graph(&graph)
        ));
        // The first page of an empty pane is on screen as it is added, before the handler was.
        if graph.holder.is_mapped() {
            self.start_graph(&graph);
        }
        self.docs.borrow_mut().push(Doc::Graph(graph.clone()));
        self.select_new_page(&graph.page, how);
        self.mark_opened(&graph.page, how);
        self.save_session_soon();
        graph
    }

    /// The graph came on screen for the first time: seeded with the file in front.
    fn start_graph(self: &Rc<Self>, graph: &Rc<GraphTab>) {
        if graph.started.replace(true) {
            return;
        }
        let front = self.graph_front().map(|(_, key)| key);
        graph.view.set_front(front.clone());
        graph.view.graph_mut().roots = front.into_iter().collect();
        if let Some(vault) = self.vault() {
            let saved = vault.session().graph;
            graph.all.set(saved.all);
            graph.shown.set(saved.shown);
        }
        self.sync_graph_actions();
        self.query_graph(graph, Ask::Fresh);
    }

    /// The graph's menu commands, over the graph in front: Reset Graph back to the file in
    /// front, the whole vault's, and the toggles of what is shown.
    pub(crate) fn graph_action(self: &Rc<Self>, name: &str) {
        let Some(graph) = self.active_doc().and_then(|doc| doc.graph().cloned()) else {
            return;
        };
        let mut shown = graph.shown.get();
        let ask = match name {
            "graph-reset" => {
                graph.all.set(false);
                let front = self.graph_front().map(|(_, key)| key);
                graph.view.graph_mut().roots = front.into_iter().collect();
                Ask::Fresh
            }
            "graph-all" => {
                graph.all.set(!graph.all.get());
                Ask::Fresh
            }
            "graph-unresolved" => {
                shown.unresolved = !shown.unresolved;
                Ask::Again
            }
            "graph-tags" => {
                shown.tags = !shown.tags;
                Ask::Again
            }
            "graph-images" => {
                shown.images = !shown.images;
                Ask::Again
            }
            "graph-orphans" => {
                shown.orphans = !shown.orphans;
                Ask::Again
            }
            _ => return,
        };
        graph.shown.set(shown);
        graph.describe();
        self.query_graph(&graph, ask);
        self.sync_graph_actions();
        self.sync_applicable();
        self.save_session_soon();
    }

    /// The graph menu's checks follow the graph in front.
    pub(crate) fn sync_graph_actions(&self) {
        let saved = (self
            .active_doc()
            .and_then(|doc| doc.graph().map(|g| g.saved())))
        .unwrap_or_default();
        let shown = saved.shown;
        for (name, on) in [
            ("graph-all", saved.all),
            ("graph-unresolved", shown.unresolved),
            ("graph-tags", shown.tags),
            ("graph-images", shown.images),
            ("graph-orphans", shown.orphans),
        ] {
            let action = self.window.lookup_action(name);
            if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
                action.set_state(&on.to_variant());
            }
        }
    }

    /// Ask the vault for what `graph` shows, off the main loop, and show the answer as `ask`
    /// says.
    fn query_graph(self: &Rc<Self>, graph: &Rc<GraphTab>, ask: Ask) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (all, shown) = (graph.all.get(), graph.shown.get());
        let roots = graph.view.graph().roots.clone();
        if !all && roots.is_empty() {
            graph.show_empty();
            graph.describe();
            return;
        }
        let asked = graph.asked.get() + 1;
        graph.asked.set(asked);
        // A graph started afresh is kept until the answer lands, and gives way to the loading
        // page if that takes a while, as an open does.
        if ask == Ask::Fresh && loading::shown(&graph.holder) {
            let weak = Rc::downgrade(graph);
            glib::timeout_add_local_once(loading::OPENING, move || {
                let waiting = |g: &Rc<GraphTab>| g.asked.get() == asked && g.answered.get() < asked;
                if let Some(graph) = weak.upgrade().filter(waiting) {
                    graph.holder.set_visible_child_name(loading::LOADING);
                }
            });
        }
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(graph));
        let task = glib::spawn_future_local(async move {
            let answer = work::off_thread("graph", move || match all {
                true => vault.graph_all(shown),
                false => vault.graph_around(&roots, shown),
            })
            .await;
            let Some(app) = app.upgrade() else {
                return;
            };
            let Some(graph) = weak.upgrade().filter(|g| g.asked.get() == asked) else {
                return;
            };
            graph.answered.set(asked);
            match answer {
                Some(Ok(part)) => app.land_graph(&graph, &part, &ask),
                Some(Err(why)) => app.graph_failed(&graph, &why.to_string()),
                None => app.graph_failed(&graph, "the worker stopped"),
            }
        });
        if let Some(old) = graph.query.replace(Some(task)) {
            old.abort();
        }
    }

    /// The vault's answer: what changed shown, the canvas up if it was not.
    fn land_graph(self: &Rc<Self>, graph: &Rc<GraphTab>, part: &Part, ask: &Ask) {
        if *ask == Ask::Fresh {
            graph.view.restart();
        }
        let near = match ask {
            Ask::Grow(key) => Some(key.as_str()),
            Ask::Fresh | Ask::Again => None,
        };
        let changes = graph.view.graph().changes(part, near);
        graph.view.apply(&changes);
        loading::show(&graph.holder);
        graph.describe();
        if matches!(self.active_doc(), Some(Doc::Graph(_))) {
            self.sync_status();
        }
    }

    /// The answer did not come: a graph on screen stays as it is, a first one says why.
    fn graph_failed(self: &Rc<Self>, graph: &Rc<GraphTab>, why: &str) {
        if loading::shown(&graph.holder) {
            tracing::warn!("graph: {why}");
            return;
        }
        let weak = Rc::downgrade(graph);
        let app = Rc::downgrade(self);
        let retry = move || {
            if let (Some(app), Some(graph)) = (app.upgrade(), weak.upgrade()) {
                loading::again(&graph.holder);
                app.query_graph(&graph, Ask::Fresh);
            }
        };
        loading::failed(
            &graph.holder,
            "Cannot Show Graph",
            &loading::sentence(why),
            retry,
        );
    }

    /// A press on node `i`: a file opens beside the graph, and the graph grows by its
    /// neighbours; a tag grows it by its files; a ghost offers New File with its path typed in,
    /// as following its link does, and once written it is a file in the ghost's place.
    fn graph_activate(self: &Rc<Self>, graph: &Rc<GraphTab>, i: usize, keep: bool) {
        let Some(node) = graph.view.graph().nodes.get(i).cloned() else {
            return;
        };
        match node.kind {
            NodeKind::File(_) => {
                self.open_from_graph(graph, &node.key, keep);
                self.grow_graph(graph, &node.key);
            }
            NodeKind::Tag => self.grow_graph(graph, &node.key),
            // Cancelling says nothing, as following the link does.
            NodeKind::Missing if !keep => {
                if let Some(ops) = self.ops() {
                    fileops::new_linked_note(ops, &node.key);
                }
            }
            NodeKind::Missing => {}
        }
    }

    /// Add `key` to the roots, so its neighbours join the graph beside it. The whole vault's
    /// graph has nothing to add.
    fn grow_graph(self: &Rc<Self>, graph: &Rc<GraphTab>, key: &str) {
        let grew = {
            let mut shown = graph.view.graph_mut();
            let new = !graph.all.get() && !shown.roots.iter().any(|r| r == key);
            if new {
                shown.roots.push(key.to_string());
            }
            new
        };
        if grew {
            graph.describe();
            self.query_graph(graph, Ask::Grow(key.to_string()));
        }
    }

    /// Open `rel` beside `graph`, as the preview of the most recently used other pane, so a walk
    /// down the graph leaves one tab there; in a window of one pane, in a pane split off to the
    /// graph's right. Open in another pane already, it is brought to the front there, and open
    /// behind the graph, it moves over. A single press leaves the keyboard on the graph, as a
    /// row of the Files tree does; `keep`, a double click, keeps the tab and gives it the
    /// keyboard.
    pub(crate) fn open_from_graph(self: &Rc<Self>, graph: &GraphTab, rel: &str, keep: bool) {
        let Some(home) = self.pane_of(&graph.page) else {
            return;
        };
        let other = (self.window_order().iter())
            .filter_map(|page| self.pane_of(page))
            .find(|pane| !Rc::ptr_eq(pane, &home));
        let open = (self.doc_for(rel).map(|doc| doc.page().clone())).or_else(|| self.editing(rel));
        match (open, other) {
            (Some(page), _) if self.pane_of(&page).is_some_and(|p| !Rc::ptr_eq(&p, &home)) => {
                if let Some(pane) = self.pane_of(&page) {
                    pane.tabs.set_selected_page(&page);
                }
            }
            // Pinned where it is, it is shown there.
            (Some(page), _) if self.is_pinned(&page) => return self.reveal_page(&page),
            (Some(page), Some(other)) => {
                home.tabs
                    .transfer_page(&page, &other.tabs, other.tabs.n_pages());
                other.tabs.set_selected_page(&page);
            }
            (Some(page), None) => self.split_page(&home, Side::Right, &page),
            (None, Some(other)) => {
                self.pending.place(rel, &other);
                self.open_preview(rel);
            }
            (None, None) => {
                let pane = self.split_beside(&home, Side::Right);
                self.pending.place(rel, &pane);
                self.open_preview(rel);
                // Not opened after all, and said why: the pane stands empty.
                if pane.tabs.n_pages() == 0 {
                    self.close_pane(&pane);
                }
            }
        }
        let landed =
            (self.doc_for(rel).map(|doc| doc.page().clone())).or_else(|| self.editing(rel));
        match (keep, landed) {
            (true, Some(page)) => {
                self.promote(&page);
                self.reveal_page(&page);
                if let Some(pane) = self.pane_of(&page) {
                    self.focus_document(&pane);
                }
            }
            (true, None) => self.pending.keep(rel),
            (false, _) => {
                if self.set_active_pane(&home) {
                    self.sync_active();
                }
                // The file opened is the one in front now, which the accent follows.
                self.sync_graphs();
                // After any focus a split queued, which an idle of its own hands over.
                let view = graph.view.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(view) = view.upgrade() {
                        view.grab_focus();
                    }
                });
            }
        }
    }

    /// The file the graph is around: of the window's tabs, most recently used first, the first
    /// that is a vault file, a preview tab's note and a terminal editor's file among them, and
    /// the tab holding it.
    pub(crate) fn graph_front(&self) -> Option<(adw::TabPage, String)> {
        self.window_order().into_iter().find_map(|page| {
            let doc = self.doc_for_page(&page)?;
            let key = rendered::note_for(doc)?.file()?;
            (!doc::is_loose_key(&key)).then_some((page, key))
        })
    }

    /// The window's graph, in a list as the other kinds are.
    pub(crate) fn graphs(&self) -> Vec<Rc<GraphTab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.graph().cloned())
            .collect()
    }

    /// The graph's accent follows the file in front, without changing what it shows.
    pub(crate) fn sync_graphs(&self) {
        let graphs = self.graphs();
        if graphs.is_empty() {
            return;
        }
        let front = self.graph_front().map(|(_, key)| key);
        for graph in graphs {
            graph.view.set_front(front.clone());
        }
    }

    /// The vault changed: a file renamed keeps its node under its new name, a root deleted is no
    /// root, and each graph asks again [`REFRESH`] after the last change, a ghost whose note was
    /// written turning into it in place.
    pub(crate) fn graphs_follow(self: &Rc<Self>, event: &Event) {
        for graph in self.graphs().iter().filter(|g| g.started.get()) {
            match event {
                Event::FileRenamed { from, to } => graph.view.rename(from, to),
                Event::FileRemoved(rel) => graph.view.graph_mut().forget(rel),
                _ => {}
            }
            self.refresh_graph_soon(graph);
        }
    }

    /// Ask again after [`REFRESH`], as the vault changed: an own save emits no event.
    pub(crate) fn refresh_graphs_soon(self: &Rc<Self>) {
        for graph in self.graphs().iter().filter(|g| g.started.get()) {
            self.refresh_graph_soon(graph);
        }
    }

    fn refresh_graph_soon(self: &Rc<Self>, graph: &Rc<GraphTab>) {
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(graph));
        graph.refresh.call(move || {
            if let (Some(app), Some(graph)) = (app.upgrade(), weak.upgrade()) {
                graph.describe();
                app.query_graph(&graph, Ask::Again);
            }
        });
    }

    /// Lay the graphs' names out again in a new style.
    pub(crate) fn restyle_graphs(&self) {
        for graph in self.graphs() {
            graph.view.restyle();
        }
    }
}
