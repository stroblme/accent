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
use accent_core::graph::layout::Spread;
use accent_core::graph::{NodeKind, Part, Shown};
use accent_core::search::{self, Options};
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

/// The spread slider's ends, from half d3's spacing to three times it, and the step it moves in,
/// which keeps each value exact, the default's 1 among them.
const SPREAD: (f64, f64, f64) = (0.5, 3.0, 0.25);

pub(crate) fn is_key(key: &str) -> bool {
    key == KEY
}

/// The find bar's query over the nodes' names: the keys of the nodes it hits, by name.
#[derive(Default)]
struct Find {
    query: String,
    invalid: bool,
    hits: Vec<String>,
    current: Option<usize>,
}

/// The graph's menu: Show Whole Vault; what is shown, Show Orphans while the whole vault is,
/// its action being off otherwise; Reset Graph. A toggle is drawn as a check, its action carrying
/// its state.
fn graph_menu() -> gio::Menu {
    let section = |actions: &[&str]| {
        let section = gio::Menu::new();
        for action in actions {
            let item = gio::MenuItem::new(Some(crate::actions::label_of(action)), Some(action));
            if *action == "win.graph-orphans" {
                item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            }
            section.append_item(&item);
        }
        section
    };
    let menu = gio::Menu::new();
    menu.append_section(None, &section(&["win.graph-all"]));
    menu.append_section(
        None,
        &section(&[
            "win.graph-unresolved",
            "win.graph-tags",
            "win.graph-images",
            "win.graph-orphans",
        ]),
    );
    menu.append_section(None, &section(&["win.graph-reset"]));
    menu
}

/// The graph's `menu` at `at` over `host`, as `widgets::popup_menu` shows it, with the keyboard
/// left on the graph once it closes. The press that opened it made the graph's pane the active
/// one, while the keyboard `popup_menu` gives back may be in another pane, which would then be
/// the active one again; the canvas takes it instead, the button keeping it after a keyboard's
/// open.
fn popup(
    host: &gtk::Box,
    menu: &gio::Menu,
    at: gdk::Rectangle,
    view: &GraphView,
) -> gtk::PopoverMenu {
    let popover = widgets::popup_menu(host, menu, Some(at));
    // After `popup_menu`'s own handler, which has given the keyboard back.
    popover.connect_closed(glib::clone!(
        #[weak]
        host,
        #[weak]
        view,
        move |_| {
            let focus = host.root().and_then(|root| root.focus());
            if !focus.is_some_and(|focus| focus.is_ancestor(&host)) {
                view.grab_focus();
            }
        }
    ));
    popover
}

/// The menu's button over the canvas's top right corner, a disc as opaque as the ring's buttons
/// over a page (DESIGN.md, PDF). Its menu holds Spread above Reset Graph, `spread` being the
/// slider's value, and hangs from `host` as the right-click's does, pointing at the button: once
/// closed, a popover of the button's own left the keyboard on the button rather than where it
/// was.
fn menu_button(spread: &gtk::Adjustment, host: &gtk::Box, view: &GraphView) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Graph Menu")
        .focus_on_click(false)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_end(12)
        .build();
    // Added, so the class the icon gave the button stays.
    for class in ["circular", "opaque", "accent-floating"] {
        button.add_css_class(class);
    }
    button.update_property(&[
        gtk::accessible::Property::Label("Graph Menu"),
        gtk::accessible::Property::HasPopup(true),
    ]);
    let spread = spread.clone();
    button.connect_clicked(glib::clone!(
        #[weak]
        host,
        #[weak]
        view,
        move |button| {
            let Some(at) = button.compute_bounds(&host) else {
                return;
            };
            let menu = graph_menu();
            let item = gio::MenuItem::new(None, None);
            item.set_attribute_value("custom", Some(&"spread".to_variant()));
            let slider = gio::Menu::new();
            slider.append_item(&item);
            menu.insert_section(2, None, &slider);
            let at = gdk::Rectangle::new(
                at.x() as i32,
                at.y() as i32,
                at.width() as i32,
                at.height() as i32,
            );
            popup(&host, &menu, at, &view).add_child(&spread_row(&spread), "spread");
        }
    ));
    button
}

/// Spread's row in the menu: its name in line with the items' labels, past where their checks
/// go (Adwaita's 12 px padding and 22 px check), and the slider, the default marked.
fn spread_row(spread: &gtk::Adjustment) -> gtk::Box {
    let scale = gtk::Scale::builder()
        .adjustment(spread)
        .width_request(128)
        .hexpand(true)
        .build();
    scale.add_mark(Spread::default().0.into(), gtk::PositionType::Bottom, None);
    // A drag stops on the steps the keyboard takes, so it can land on the default again.
    scale.connect_change_value(|scale, _, value| {
        let step = SPREAD.2;
        scale.set_value((value / step).round() * step);
        glib::Propagation::Stop
    });
    scale.update_property(&[gtk::accessible::Property::Label("Spread")]);
    let row = gtk::Box::builder()
        .spacing(12)
        .margin_start(34)
        .margin_end(12)
        .build();
    row.append(&gtk::Label::new(Some("Spread")));
    row.append(&scale);
    row
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

impl Ask {
    /// One question for two, the older's answer not shown: starting afresh wins over a grow, and
    /// a grow over asking again.
    fn and(self, newer: Ask) -> Ask {
        match (self, newer) {
            (Ask::Fresh, _) | (_, Ask::Fresh) => Ask::Fresh,
            (_, Ask::Grow(key)) => Ask::Grow(key),
            (older, Ask::Again) => older,
        }
    }
}

/// The tab's questions to the vault, one on its way at a time: whatever is asked meanwhile waits
/// for it as one question, the newest, and the answer on its way is then not shown. So toggles
/// pressed in a row or saves in a burst leave no backlog on the vault's workers, which go on with
/// a question once it is asked.
#[derive(Default)]
struct Asking {
    /// What the question on its way is for.
    running: Option<Ask>,
    /// The question asked since, for all those it stands for.
    waiting: Option<Ask>,
    /// The vault changed while the graph was hidden: it asks again once it is shown.
    stale: bool,
}

impl Asking {
    /// `ask` asked: what to ask the vault now, or nothing while a question is on its way.
    fn ask(&mut self, ask: Ask) -> Option<Ask> {
        self.stale = false;
        if self.running.is_some() {
            self.waiting = Some(match self.waiting.take() {
                Some(waiting) => waiting.and(ask),
                None => ask,
            });
            return None;
        }
        self.running = Some(ask.clone());
        Some(ask)
    }

    /// The question on its way answered: nothing to show its answer, else what to ask instead,
    /// for what both were for.
    fn answered(&mut self) -> Option<Ask> {
        let (running, waiting) = (self.running.take(), self.waiting.take());
        self.running = match (running, waiting) {
            (Some(running), Some(waiting)) => Some(running.and(waiting)),
            (_, waiting) => waiting,
        };
        self.running.clone()
    }
}

pub struct GraphTab {
    pub page: adw::TabPage,
    /// The page's child: blank, then loading, then the canvas, the empty page or a failure.
    holder: gtk::Stack,
    view: GraphView,
    /// The whole vault rather than the roots and their neighbours.
    all: Cell<bool>,
    shown: Cell<Shown>,
    /// The spread slider's value, which the canvas lays the nodes out at.
    spread: gtk::Adjustment,
    /// The question on its way to the vault and the one waiting for it.
    asking: RefCell<Asking>,
    /// Counts the questions asked, so only the last one's answer lands.
    asked: Cell<u64>,
    /// Whether it has been seeded, which waits until it is first on screen.
    started: Cell<bool>,
    /// The question asked again after the vault changed ([`REFRESH`]).
    refresh: widgets::Debounce,
    /// The find bar's query over the nodes' names, and the nodes it hits.
    finding: RefCell<Find>,
    /// The last question whose answer landed.
    answered: Cell<u64>,
    /// How long the answers took to land on the main loop, the diff and its apply, for the
    /// drill.
    #[cfg(feature = "bench")]
    landing_ms: Cell<f64>,
    /// When the first answer since the landings were last taken landed, for the drill.
    #[cfg(feature = "bench")]
    landed_at: Cell<Option<std::time::Instant>>,
    /// How much of `landing_ms` was the diff, for the drill.
    #[cfg(feature = "bench")]
    diff_ms: Cell<f64>,
    /// How many questions went to the vault, for the drill.
    #[cfg(feature = "bench")]
    questions: Cell<usize>,
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

    /// What the session keeps of it: what its menu shows and its spread.
    pub fn saved(&self) -> Saved {
        Saved {
            all: self.all.get(),
            shown: self.shown.get(),
            spread: Spread(self.spread.value() as f32),
        }
    }

    /// Whether it shows the whole vault, under which Show Orphans applies.
    pub fn all(&self) -> bool {
        self.all.get()
    }

    /// Search the nodes' names for `text` under `options`, as the Search pane matches, and put
    /// the keyboard on the first hit from the node it is on, by name and round past the last.
    pub fn find(&self, text: &str, options: Options) {
        let compiled = (!text.is_empty()).then(|| search::pattern(text, options));
        let mut hits: Vec<(String, String)> = match &compiled {
            Some(Ok(re)) => (self.view.graph().nodes.iter())
                .map(|n| (model::label(n).to_string(), n.key.clone()))
                .filter(|(label, _)| re.is_match(label))
                .collect(),
            _ => Vec::new(),
        };
        hits.sort();
        *self.finding.borrow_mut() = Find {
            query: text.to_string(),
            invalid: matches!(compiled, Some(Err(_))),
            hits: hits.into_iter().map(|(_, key)| key).collect(),
            current: None,
        };
        self.step_match(true);
    }

    /// The next hit or the one before, round past either end; with none shown yet, the first on
    /// or after the node the keyboard is on.
    pub fn step_match(&self, forward: bool) {
        let next = {
            let find = self.finding.borrow();
            let total = find.hits.len();
            match (total, find.current) {
                (0, _) => None,
                (_, None) => {
                    let graph = self.view.graph();
                    let name = |key: &str| {
                        let node = graph.index_of(key).map(|i| &graph.nodes[i]);
                        node.map(|n| (model::label(n).to_string(), key.to_string()))
                    };
                    let here = self.view.current().and_then(|key| name(&key));
                    let after = (here.as_ref()).map_or(0, |here| {
                        find.hits
                            .partition_point(|key| name(key).as_ref() < Some(here))
                    });
                    Some(match forward {
                        true => after % total,
                        false => (after + total - 1) % total,
                    })
                }
                (_, Some(at)) if forward => Some((at + 1) % total),
                (_, Some(at)) => Some((at + total - 1) % total),
            }
        };
        let Some(at) = next else {
            return;
        };
        let key = {
            let mut find = self.finding.borrow_mut();
            find.current = Some(at);
            find.hits[at].clone()
        };
        self.view.set_current(&key, true);
    }

    /// Forget the query, the bar being put away; the keyboard stays on the node it found.
    pub fn find_clear(&self) {
        *self.finding.borrow_mut() = Find::default();
    }

    /// "3 of 12", or what to say when there is nothing to show.
    pub fn matches_label(&self) -> String {
        let find = self.finding.borrow();
        match (find.hits.len(), find.current) {
            _ if find.invalid => "Invalid pattern".to_string(),
            _ if find.query.is_empty() => String::new(),
            (0, _) => "No results".to_string(),
            (total, None) => format!("{total} matches"),
            (total, Some(at)) => format!("{} of {total}", at + 1),
        }
    }

    /// Show the empty page: no file to start from, and not the whole vault.
    fn show_empty(&self) {
        let none = self.view.graph().changes(&Part::default(), None);
        self.view.apply(&none, None);
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

    /// The node the keyboard is on.
    pub fn current(&self) -> Option<String> {
        self.view.current()
    }

    /// A key pressed over the canvas.
    pub fn press_key(&self, key: gdk::Key) {
        self.view.press_key(key);
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

    /// How long the answers since the last call took to land on the main loop, in ms, and how
    /// much of that was the diff.
    pub fn take_landing_ms(&self) -> (f64, f64) {
        self.landed_at.take();
        (self.landing_ms.replace(0.0), self.diff_ms.replace(0.0))
    }

    /// How many questions went to the vault so far.
    pub fn questions(&self) -> usize {
        self.questions.get()
    }

    /// When the first answer since [`GraphTab::take_landing_ms`] landed.
    pub fn landed_at(&self) -> Option<std::time::Instant> {
        self.landed_at.get()
    }

    /// The canvas's frame clock, once it is on screen.
    pub fn frame_clock(&self) -> Option<gdk::FrameClock> {
        self.view.frame_clock()
    }

    /// Input `k` of a run, as the pointer gives it: a pan, a zoom or a hover in turn.
    pub fn bench_input(&self, k: usize) {
        self.view.bench_input(k);
    }

    /// The serial of the last view asked of the worker, and of the picture shown.
    pub fn serials(&self) -> (u64, u64) {
        self.view.serials()
    }

    /// How long each frame drawn since the last call took, in ms.
    pub fn take_paints(&self) -> Vec<f64> {
        self.view.take_paints()
    }

    /// How long the worker took to draw each picture shown since the last call, in ms.
    pub fn take_draws(&self) -> Vec<f64> {
        self.view.take_draws()
    }

    /// When a picture of what is shown was first on screen.
    pub fn shown_at(&self) -> Option<std::time::Instant> {
        self.view.shown_at()
    }

    pub fn node_count(&self) -> usize {
        self.view.graph().nodes.len()
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
        // What the vault's session keeps of the graph, taken before anything saves it again.
        let saved = (self.vault().map(|vault| vault.session().graph)).unwrap_or_default();
        let (lower, upper, step) = SPREAD;
        let spread = gtk::Adjustment::new(saved.spread.0.into(), lower, upper, step, step, 0.0);
        view.set_spread(Spread(spread.value() as f32));
        // In a box its menus hang from: a popover on the canvas itself would be allocated by a
        // widget with no layout manager (DESIGN.md, States).
        let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let canvas = gtk::Overlay::builder().child(&view).build();
        canvas.add_overlay(&menu_button(&spread, &host, &view));
        host.append(&canvas);
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
            all: Cell::new(saved.all),
            shown: Cell::new(saved.shown),
            spread,
            asking: RefCell::default(),
            asked: Cell::new(0),
            started: Cell::new(false),
            refresh: widgets::Debounce::new(REFRESH),
            finding: RefCell::new(Find::default()),
            answered: Cell::new(0),
            #[cfg(feature = "bench")]
            landing_ms: Cell::new(0.0),
            #[cfg(feature = "bench")]
            landed_at: Cell::new(None),
            #[cfg(feature = "bench")]
            diff_ms: Cell::new(0.0),
            #[cfg(feature = "bench")]
            questions: Cell::new(0),
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
        // Typing over the canvas opens its pane's find bar on what is typed: the key goes on to
        // the bar's entry, which composes it as it would have.
        let weak = Rc::downgrade(&graph);
        graph.view.connect_typed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |keys| {
                let pane = weak.upgrade().and_then(|graph| app.pane_of(&graph.page));
                if let Some(pane) = pane {
                    pane.find.open_typed(keys);
                }
            }
        ));
        graph.view.connect_zoomed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || app.refresh_zoom()
        ));
        let weak = Rc::downgrade(&graph);
        graph.spread.connect_value_changed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |spread| {
                if let Some(graph) = weak.upgrade() {
                    graph.view.set_spread(Spread(spread.value() as f32));
                    app.save_session_soon();
                }
            }
        ));
        // Its menu, anywhere on the canvas.
        let menu = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let view = graph.view.clone();
        menu.connect_pressed(glib::clone!(
            #[weak]
            host,
            #[weak]
            view,
            move |menu, _, x, y| {
                menu.set_state(gtk::EventSequenceState::Claimed);
                let anchor = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                popup(&host, &graph_menu(), anchor, &view);
            }
        ));
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

    /// The graph came on screen for the first time: seeded with the file in front. Again, it
    /// asks what changed in the vault while it was hidden.
    fn start_graph(self: &Rc<Self>, graph: &Rc<GraphTab>) {
        if graph.started.replace(true) {
            let stale = std::mem::take(&mut graph.asking.borrow_mut().stale);
            if stale {
                graph.describe();
                self.query_graph(graph, Ask::Again);
            }
            return;
        }
        let front = self.graph_front().map(|(_, key)| key);
        graph.view.set_front(front.clone());
        graph.view.graph_mut().roots = front.into_iter().collect();
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
    /// says: at once, or once the question on its way is answered ([`Asking`]).
    fn query_graph(self: &Rc<Self>, graph: &Rc<GraphTab>, ask: Ask) {
        if self.vault().is_none() {
            return;
        }
        let asked = graph.asked.get() + 1;
        graph.asked.set(asked);
        if !graph.all.get() && graph.view.graph().roots.is_empty() {
            graph.asking.borrow_mut().waiting = None;
            graph.show_empty();
            graph.describe();
            return;
        }
        // A graph started afresh is kept until the answer lands, and gives way to the loading
        // page if that takes a while, as an open does.
        if ask == Ask::Fresh && loading::shown(&graph.holder) {
            let weak = Rc::downgrade(graph);
            glib::timeout_add_local_once(loading::OPENING, move || {
                let waiting =
                    |g: &Rc<GraphTab>| g.answered.get() < asked && loading::shown(&g.holder);
                if let Some(graph) = weak.upgrade().filter(waiting) {
                    graph.holder.set_visible_child_name(loading::LOADING);
                }
            });
        }
        let now = graph.asking.borrow_mut().ask(ask);
        if let Some(ask) = now {
            self.send_graph_query(graph, ask);
        }
    }

    /// Put the question for what `graph` shows now on its way, and once it is answered show the
    /// answer, or ask the question that waited for it instead.
    fn send_graph_query(self: &Rc<Self>, graph: &Rc<GraphTab>, ask: Ask) {
        let Some(vault) = self.vault().cloned() else {
            *graph.asking.borrow_mut() = Asking::default();
            return;
        };
        let (all, shown) = (graph.all.get(), graph.shown.get());
        let roots = graph.view.graph().roots.clone();
        let asked = graph.asked.get();
        // The answer is diffed beside the query, against the graph shown now, or against none
        // for a fresh start, which empties the graph as it lands.
        let snapshot = match ask {
            Ask::Fresh => (0, Vec::new()),
            _ => graph.view.graph().snapshot(),
        };
        let near = match &ask {
            Ask::Grow(key) => Some(key.clone()),
            Ask::Fresh | Ask::Again => None,
        };
        #[cfg(feature = "bench")]
        graph.questions.set(graph.questions.get() + 1);
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(graph));
        glib::spawn_future_local(async move {
            let answer = work::off_thread("graph", move || {
                let part = match all {
                    true => vault.graph_all(shown),
                    false => vault.graph_around(&roots, shown),
                };
                part.map(|part| model::Ready::new(snapshot, part, near.as_deref()))
            })
            .await;
            let (Some(app), Some(graph)) = (app.upgrade(), weak.upgrade()) else {
                return;
            };
            let next = graph.asking.borrow_mut().answered();
            if let Some(next) = next {
                return app.send_graph_query(&graph, next);
            }
            // The empty page went up meanwhile.
            if graph.asked.get() != asked {
                return;
            }
            graph.answered.set(asked);
            match answer {
                Some(Ok(ready)) => app.land_graph(&graph, ready, &ask),
                Some(Err(why)) => app.graph_failed(&graph, &why.to_string()),
                None => app.graph_failed(&graph, "the worker stopped"),
            }
        });
    }

    /// The vault's answer: what changed shown, the canvas up if it was not. Made ready against
    /// a graph that has changed since, by a rename say, it is diffed again here.
    fn land_graph(self: &Rc<Self>, graph: &Rc<GraphTab>, ready: model::Ready, ask: &Ask) {
        #[cfg(feature = "bench")]
        let t = std::time::Instant::now();
        #[cfg(feature = "bench")]
        if graph.landed_at.get().is_none() {
            graph.landed_at.set(Some(t));
        }
        if *ask == Ask::Fresh {
            graph.view.restart();
        }
        let near = match ask {
            Ask::Grow(key) => Some(key.as_str()),
            Ask::Fresh | Ask::Again => None,
        };
        #[cfg(feature = "bench")]
        let d = std::time::Instant::now();
        let (changes, next) = match ready.version == graph.view.graph().version() {
            true => (ready.changes, Some(ready.graph)),
            false => (graph.view.graph().changes(&ready.part, near), None),
        };
        #[cfg(feature = "bench")]
        graph
            .diff_ms
            .set(graph.diff_ms.get() + d.elapsed().as_secs_f64() * 1000.0);
        graph.view.apply(&changes, next);
        loading::show(&graph.holder);
        graph.describe();
        if matches!(self.active_doc(), Some(Doc::Graph(_))) {
            self.sync_status();
        }
        #[cfg(feature = "bench")]
        graph
            .landing_ms
            .set(graph.landing_ms.get() + t.elapsed().as_secs_f64() * 1000.0);
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

    /// The graph grown by `key` as a click grows it, without the file opening.
    #[cfg(feature = "bench")]
    pub(crate) fn bench_grow(self: &Rc<Self>, graph: &Rc<GraphTab>, key: &str) {
        self.grow_graph(graph, key);
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

    /// The graph in front of `pane`, if that is one.
    pub(crate) fn graph_of(&self, pane: &Pane) -> Option<Rc<GraphTab>> {
        self.doc_of(pane)?.graph().cloned()
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

    /// Hidden behind another tab, the graph asks once it is shown again ([`App::start_graph`]).
    fn refresh_graph_soon(self: &Rc<Self>, graph: &Rc<GraphTab>) {
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(graph));
        graph.refresh.call(move || {
            let (Some(app), Some(graph)) = (app.upgrade(), weak.upgrade()) else {
                return;
            };
            if !graph.holder.is_mapped() {
                graph.asking.borrow_mut().stale = true;
                return;
            }
            graph.describe();
            app.query_graph(&graph, Ask::Again);
        });
    }

    /// Lay the graphs' names out again in a new style.
    pub(crate) fn restyle_graphs(&self) {
        for graph in self.graphs() {
            graph.view.restyle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ask, Asking};

    /// One question on its way at a time: those asked meanwhile wait as one, for what each was
    /// for, and the answer on its way gives way to it.
    #[test]
    fn questions_asked_meanwhile_wait_as_one() {
        let mut asking = Asking::default();
        assert_eq!(asking.ask(Ask::Again), Some(Ask::Again));
        // A burst of saves.
        for _ in 0..5 {
            assert_eq!(asking.ask(Ask::Again), None);
        }
        assert_eq!(asking.answered(), Some(Ask::Again));
        assert_eq!(asking.answered(), None);
        // A grow on its way keeps its node to slide out of, and a fresh start wins.
        assert_eq!(
            asking.ask(Ask::Grow("a.md".into())),
            Some(Ask::Grow("a.md".into()))
        );
        asking.ask(Ask::Again);
        assert_eq!(asking.answered(), Some(Ask::Grow("a.md".into())));
        asking.ask(Ask::Fresh);
        asking.ask(Ask::Again);
        assert_eq!(asking.answered(), Some(Ask::Fresh));
        assert_eq!(asking.answered(), None);
    }
}
